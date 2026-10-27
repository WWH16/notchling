//! The Claude desktop app's own login, read but never refreshed or written.
//!
//! The desktop app signs in apart from the Claude Code CLI, and its Code tab runs Claude
//! Code on that login, so switching accounts in the app leaves
//! `~/.claude/.credentials.json` untouched. The app keeps its tokens in `config.json`,
//! sealed the way Chromium seals secrets: AES-256-GCM under a key in `Local State`,
//! which DPAPI unseals for the current Windows user only.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::mem;
use std::path::{Path, PathBuf};
use std::ptr;
use std::slice;

use anyhow::{Context, anyhow, ensure};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;
use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Cryptography::{
    BCRYPT_AES_GCM_ALG_HANDLE, BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO,
    BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO_VERSION, BCRYPT_FLAGS, BCRYPT_KEY_HANDLE, BCryptDecrypt,
    BCryptDestroyKey, BCryptGenerateSymmetricKey, CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN,
    CryptUnprotectData,
};

use super::claude_account::{REFRESH_MARGIN_MS, Secret};

/// Chromium's marker in front of data sealed with its AES key.
const SEALED_PREFIX: &[u8] = b"v10";

/// Chromium's marker in front of its AES key sealed with DPAPI.
const KEY_PREFIX: &[u8] = b"DPAPI";

const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;

/// The scope the usage endpoint needs.
const PROFILE_SCOPE: &str = "user:profile";

/// Reads the login of the current Windows user's Claude desktop app.
#[derive(Debug)]
pub struct ClaudeDesktop {
    data: PathBuf,
}

impl ClaudeDesktop {
    /// Uses the data folder of the desktop app from the Microsoft Store, the one
    /// installer Anthropic ships for Windows as of October 2026.
    ///
    /// # Errors
    ///
    /// Fails when `LOCALAPPDATA` is not set.
    pub fn try_new() -> anyhow::Result<Self> {
        let local = env::var_os("LOCALAPPDATA").context("LOCALAPPDATA is not set")?;
        Ok(Self::at(
            Path::new(&local)
                .join("Claude-Data")
                .join("claude_pzs8sxrjxfjjc"),
        ))
    }

    fn at(data: PathBuf) -> Self {
        Self { data }
    }

    /// The account the app is signed in to. Nothing is unsealed, so this is cheap
    /// enough to check on every poll.
    pub fn account(&self) -> Option<String> {
        self.config().ok()?.last_known_account_uuid
    }

    /// The access token of the account the app is signed in to, unless it has expired.
    ///
    /// # Errors
    ///
    /// Fails when the app is not installed or not signed in, when its login cannot be
    /// unsealed, or when it holds no current token for the account.
    pub fn access_token(&self, now: DateTime<Utc>) -> anyhow::Result<Secret> {
        let config = self.config()?;
        let account = config
            .last_known_account_uuid
            .context("the Claude desktop app is not signed in")?;
        let sealed = config
            .token_cache
            .context("the Claude desktop app holds no login")?;
        let cache = open(&self.key()?, &decode(&sealed)?)?;
        pick(&cache, &account, now)
    }

    fn config(&self) -> anyhow::Result<Config> {
        let path = self.data.join("config.json");
        let text =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        // serde_json errors can quote values from the file, so they are dropped.
        serde_json::from_str(&text)
            .map_err(|_| anyhow!("the Claude desktop app's config has an unexpected format"))
    }

    /// The app's AES key, unsealed with DPAPI.
    fn key(&self) -> anyhow::Result<Vec<u8>> {
        let path = self.data.join("Local State");
        let text =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let state: LocalState = serde_json::from_str(&text)
            .map_err(|_| anyhow!("the Claude desktop app's key has an unexpected format"))?;
        let sealed = decode(&state.os_crypt.encrypted_key)?;
        let sealed = sealed
            .strip_prefix(KEY_PREFIX)
            .context("the Claude desktop app's key is not sealed with DPAPI")?;
        unprotect(sealed)
    }
}

/// The fields of the app's `config.json` this source reads.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Config {
    last_known_account_uuid: Option<String>,
    /// Base64 of the sealed token cache.
    #[serde(rename = "oauth:tokenCacheV2")]
    token_cache: Option<String>,
}

#[derive(Deserialize)]
struct LocalState {
    os_crypt: OsCrypt,
}

#[derive(Deserialize)]
struct OsCrypt {
    /// Base64 of `DPAPI` and the sealed key.
    encrypted_key: String,
}

/// One login in the app's token cache.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedLogin {
    token: Secret,
    /// Unix time in milliseconds.
    expires_at: Option<i64>,
}

fn decode(text: &str) -> anyhow::Result<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .context("the Claude desktop app's login is not base64")
}

/// Unseals `sealed` with DPAPI, which only works for the Windows user who sealed it.
fn unprotect(sealed: &[u8]) -> anyhow::Result<Vec<u8>> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(sealed.len()).context("the sealed key is too large")?,
        // DPAPI only reads its input.
        pbData: sealed.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    // SAFETY: `input` points at `sealed`, which outlives the call; on success DPAPI
    // allocates `output` with LocalAlloc, which is copied out and freed exactly once.
    unsafe {
        CryptUnprotectData(
            &input,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
        .context("unsealing the Claude desktop app's key")?;
        let key = slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        LocalFree(Some(HLOCAL(output.pbData.cast())));
        Ok(key)
    }
}

/// Opens data Chromium sealed with `key`: `v10`, a 12-byte nonce, the ciphertext and a
/// 16-byte tag. Fails unless the tag proves the data intact.
fn open(key: &[u8], sealed: &[u8]) -> anyhow::Result<Vec<u8>> {
    let body = sealed
        .strip_prefix(SEALED_PREFIX)
        .context("the Claude desktop app's login is sealed in an unknown format")?;
    ensure!(
        body.len() >= NONCE_LEN + TAG_LEN,
        "the Claude desktop app's login is cut short"
    );
    let (nonce, rest) = body.split_at(NONCE_LEN);
    let (ciphertext, tag) = rest.split_at(rest.len() - TAG_LEN);
    let info = BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO {
        cbSize: mem::size_of::<BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO>() as u32,
        dwInfoVersion: BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO_VERSION,
        // Decrypting only reads the nonce and the tag.
        pbNonce: nonce.as_ptr().cast_mut(),
        cbNonce: NONCE_LEN as u32,
        pbTag: tag.as_ptr().cast_mut(),
        cbTag: TAG_LEN as u32,
        ..Default::default()
    };
    let mut plain = vec![0; ciphertext.len()];
    let mut written = 0u32;
    // SAFETY: the pseudo-handle needs no opening; the key object is allocated by CNG and
    // destroyed exactly once, before any early return; `info` and the slices it points
    // into outlive the decrypt call, and `plain` is as long as the ciphertext.
    unsafe {
        let mut handle = BCRYPT_KEY_HANDLE::default();
        BCryptGenerateSymmetricKey(BCRYPT_AES_GCM_ALG_HANDLE, &mut handle, None, key, 0)
            .ok()
            .context("loading the Claude desktop app's key")?;
        let status = BCryptDecrypt(
            handle,
            Some(ciphertext),
            Some(ptr::from_ref(&info).cast()),
            None,
            Some(&mut plain),
            &mut written,
            BCRYPT_FLAGS(0),
        );
        let _ = BCryptDestroyKey(handle);
        status
            .ok()
            .context("opening the Claude desktop app's login")?;
    }
    plain.truncate(written as usize);
    Ok(plain)
}

/// The longest-lived current token for `account` that can read its usage, from the
/// opened token cache. Its keys read `acct:<account>|<client>:<org>:<api>:<scopes>`.
// ponytail: an account in several organizations gets whichever token lasts longest;
// match the org of Claude Code's `oauthAccount` if that ever picks the wrong one.
fn pick(cache: &[u8], account: &str, now: DateTime<Utc>) -> anyhow::Result<Secret> {
    // serde_json errors can quote values from the cache, which holds tokens.
    let cache: BTreeMap<String, Value> = serde_json::from_slice(cache)
        .map_err(|_| anyhow!("the Claude desktop app's login has an unexpected format"))?;
    let prefix = format!("acct:{account}|");
    let now = now.timestamp_millis();
    cache
        .into_iter()
        .filter(|(key, _)| {
            key.starts_with(&prefix) && scopes(key).any(|scope| scope == PROFILE_SCOPE)
        })
        .filter_map(|(_, login)| CachedLogin::deserialize(login).ok())
        .filter(|login| {
            login
                .expires_at
                .is_none_or(|expires| expires - REFRESH_MARGIN_MS > now)
        })
        .max_by_key(|login| login.expires_at.unwrap_or(i64::MAX))
        .map(|login| login.token)
        .context("the Claude desktop app holds no current login for its account")
}

/// The scopes at the end of a token cache key, after the API's `https://` origin.
fn scopes(key: &str) -> impl Iterator<Item = &str> {
    key.split_once("://")
        .and_then(|(_, rest)| rest.split_once(':'))
        .map(|(_, scopes)| scopes)
        .unwrap_or_default()
        .split_whitespace()
}

#[cfg(test)]
mod tests {
    use std::process;

    use super::*;

    /// `{"synthetic":"desktop login"}` sealed under the key `0, 1, .. 31`.
    const SEALED: &str =
        "djEwoKGio6SlpqeoqaqrnToPVCu/atoWDOTxPVikuwPHLX/ily4D+2dIpALRInFfrUCc5LjQR+p8XXTo";

    fn test_key() -> Vec<u8> {
        (0..32).collect()
    }

    #[test]
    fn sealed_login_opens_only_when_intact() {
        let sealed = decode(SEALED).expect("fixture is base64");
        let plain = open(&test_key(), &sealed).expect("fixture opens");
        assert_eq!(plain, br#"{"synthetic":"desktop login"}"#);

        let mut tampered = sealed.clone();
        *tampered.last_mut().expect("fixture is not empty") ^= 1;
        assert!(open(&test_key(), &tampered).is_err(), "a bad tag fails");
        assert!(open(&[7; 32], &sealed).is_err(), "a wrong key fails");
        assert!(open(&test_key(), &sealed[1..]).is_err(), "no v10 marker");
        assert!(open(&test_key(), b"v10short").is_err());
    }

    const ACCOUNT: &str = "00000000-0000-4000-8000-000000000001";

    /// A token cache as the app writes it, with synthetic tokens.
    fn cache() -> String {
        let key = |account: &str, client: &str, scopes: &str| {
            format!(
                "acct:{account}|{client}:00000000-0000-4000-8000-0000000000aa:https://api.anthropic.com:{scopes}"
            )
        };
        let other = "00000000-0000-4000-8000-000000000002";
        serde_json::json!({
            key(ACCOUNT, "client-a", "user:inference user:profile"):
                { "token": "synthetic-long", "refreshToken": "synthetic-refresh", "expiresAt": 1_800_000_000_000_i64 },
            key(ACCOUNT, "client-a", "user:inference"):
                { "token": "synthetic-no-profile", "expiresAt": 1_900_000_000_000_i64 },
            key(ACCOUNT, "client-b", "user:profile"):
                { "token": "synthetic-short", "expiresAt": 1_795_000_000_000_i64 },
            key(other, "client-a", "user:profile"):
                { "token": "synthetic-other-account", "expiresAt": 1_900_000_000_000_i64 },
        })
        .to_string()
    }

    fn at(ms: i64) -> DateTime<Utc> {
        DateTime::from_timestamp_millis(ms).expect("valid time")
    }

    #[test]
    fn picks_the_longest_lived_current_token_that_reads_usage() {
        let cache = cache();
        let token_for = |account, ms| pick(cache.as_bytes(), account, at(ms)).map(|token| token.0);

        assert_eq!(
            token_for(ACCOUNT, 1_793_000_000_000).expect("a token is current"),
            "synthetic-long"
        );
        assert!(
            token_for(ACCOUNT, 1_800_000_000_000 - REFRESH_MARGIN_MS).is_err(),
            "every token with the profile scope is about to expire"
        );
        assert!(token_for("00000000-0000-4000-8000-000000000003", 0).is_err());
    }

    #[test]
    fn tokens_never_show_in_errors() {
        let err = pick(br#"{"acct:x": "synthetic-secret"#, "x", at(0)).expect_err("malformed");
        assert!(!format!("{err:?}").contains("synthetic-secret"), "{err:?}");
    }

    #[test]
    fn signed_in_account_is_read_from_the_config() {
        let dir = env::temp_dir().join(format!("ai-notch-desktop-{}", process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp dir is writable");
        let desktop = ClaudeDesktop::at(dir.clone());
        assert_eq!(desktop.account(), None, "not installed");

        fs::write(
            dir.join("config.json"),
            format!(r#"{{"locale":"en-US","lastKnownAccountUuid":"{ACCOUNT}"}}"#),
        )
        .expect("temp file is writable");
        assert_eq!(desktop.account().as_deref(), Some(ACCOUNT));
        let err = desktop.access_token(at(0)).expect_err("no token cache");
        assert_eq!(err.to_string(), "the Claude desktop app holds no login");
        let _ = fs::remove_dir_all(dir);
    }
}
