//! Claude's exact usage limits from the account endpoint behind Claude Code's `/usage`
//! screen, signed in with the Claude desktop app's login or else Claude Code's own.
//!
//! The endpoint is unofficial. The desktop app's login comes first, so the notch
//! follows the account the app is signed in to; it is read, never refreshed. Claude
//! Code's login lives in `~/.claude/.credentials.json`, which only the Claude Code CLI
//! refreshes. So once that token expires, ai-notch refreshes it the way Claude Code does
//! and writes the new tokens back, and the CLI carries on with them.

use std::env;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow, bail};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use ureq::Agent;

use super::Usage;
use super::claude_desktop::ClaudeDesktop;
use crate::http;

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";

/// Beta flag the endpoint requires for OAuth logins.
const OAUTH_BETA: &str = "oauth-2025-04-20";

/// Where Claude Code refreshes its login, as of Claude Code 2.1.
const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";

/// The OAuth client Claude Code signs in as; a refresh must name the same client.
const CLAUDE_CODE_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";

/// A token this close to expiry is refreshed first, so it cannot lapse mid-request.
pub(super) const REFRESH_MARGIN_MS: i64 = 60_000;

/// Reads Claude's usage limits from the account endpoint.
#[derive(Debug)]
pub struct ClaudeAccount {
    desktop: ClaudeDesktop,
    credentials: PathBuf,
    agent: Agent,
}

impl ClaudeAccount {
    /// Uses the login of the current Windows user's Claude desktop app or Claude Code.
    ///
    /// # Errors
    ///
    /// Fails when `USERPROFILE` or `LOCALAPPDATA` is not set.
    pub fn try_new() -> anyhow::Result<Self> {
        let home = env::var_os("USERPROFILE").context("USERPROFILE is not set")?;
        Ok(Self {
            desktop: ClaudeDesktop::try_new()?,
            credentials: Path::new(&home).join(".claude").join(".credentials.json"),
            agent: http::agent(),
        })
    }

    /// The account the desktop app is signed in to, cheap enough to check on every
    /// poll; when it changes, the limits [`fetch`](Self::fetch) gave are another
    /// account's.
    pub fn signed_in(&self) -> Option<String> {
        self.desktop.account()
    }

    /// Asks the account endpoint for the current limits with the desktop app's login,
    /// or else with Claude Code's, refreshing that one first when it has expired.
    ///
    /// # Errors
    ///
    /// Fails when neither is signed in, when Claude Code's login cannot be refreshed,
    /// when the request fails, or when the response holds no limits.
    pub fn fetch(&self) -> anyhow::Result<Usage> {
        let now = Utc::now();
        let token = match self.desktop.access_token(now) {
            Ok(token) => token,
            Err(_) => self.access_token(now)?,
        };
        let body = self
            .agent
            .get(USAGE_URL)
            .header("Authorization", &format!("Bearer {}", token.0))
            .header("anthropic-beta", OAUTH_BETA)
            .call()
            .context("asking the usage endpoint")?
            .body_mut()
            .read_to_string()
            .context("reading the usage response")?;
        parse_usage(&body)
    }

    /// A valid access token of Claude Code's login: the stored one, or a refreshed one
    /// once it has expired.
    /// A refresh is written back to the credentials file unless Claude Code changed
    /// the file meanwhile, in which case its tokens win.
    fn access_token(&self, now: DateTime<Utc>) -> anyhow::Result<Secret> {
        let path = &self.credentials;
        let read =
            || fs::read_to_string(path).with_context(|| format!("reading {}", path.display()));
        let text = read()?;
        let login = parse_login(&text)?;
        if !login.expires_by(now) {
            return Ok(login.access_token);
        }
        let refresh_token = login
            .refresh_token
            .context("the Claude Code login expired; run `claude` to sign in again")?;
        let refreshed = self.refresh(&refresh_token, &login.scopes)?;
        let updated = updated_credentials(&text, &refreshed, now)?;
        if read()? == text {
            write_atomically(path, &updated)?;
        }
        Ok(refreshed.access_token)
    }

    /// Trades the refresh token for new tokens, exactly as Claude Code does.
    fn refresh(&self, refresh_token: &Secret, scopes: &[String]) -> anyhow::Result<Refreshed> {
        let mut request = json!({
            "grant_type": "refresh_token",
            "refresh_token": refresh_token.0,
            "client_id": CLAUDE_CODE_CLIENT_ID,
        });
        if !scopes.is_empty() {
            request["scope"] = scopes.join(" ").into();
        }
        let reply = self
            .agent
            .post(TOKEN_URL)
            .header("Content-Type", "application/json")
            .send(request.to_string())
            .context("refreshing the Claude Code login; run `claude` once if this keeps failing")?
            .body_mut()
            .read_to_string()
            .context("reading the refreshed login")?;
        // serde_json errors can quote values from the reply, which holds tokens.
        serde_json::from_str(&reply)
            .map_err(|_| anyhow!("the refreshed login has an unexpected format"))
    }
}

/// An OAuth token. `Debug` hides it, so it never reaches a log or an error.
#[derive(Clone, Deserialize)]
#[serde(transparent)]
pub(super) struct Secret(pub(super) String);

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(..)")
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Credentials {
    claude_ai_oauth: Option<OAuth>,
}

/// The login fields ai-notch uses; everything else in the file is left as it is.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OAuth {
    access_token: Secret,
    refresh_token: Option<Secret>,
    /// Unix time in milliseconds.
    expires_at: Option<i64>,
    #[serde(default)]
    scopes: Vec<String>,
}

impl OAuth {
    /// Whether the access token has expired by `now`, or is about to.
    fn expires_by(&self, now: DateTime<Utc>) -> bool {
        self.expires_at
            .is_some_and(|expires| expires - REFRESH_MARGIN_MS <= now.timestamp_millis())
    }
}

/// The Claude Code login in a credentials file.
fn parse_login(text: &str) -> anyhow::Result<OAuth> {
    // serde_json errors can quote values from the file, so they are dropped.
    let credentials: Credentials = serde_json::from_str(text)
        .map_err(|_| anyhow!("the Claude Code credentials file has an unexpected format"))?;
    credentials
        .claude_ai_oauth
        .context("Claude Code is not signed in with a Claude account")
}

/// The token endpoint's reply to a refresh.
#[derive(Debug, Deserialize)]
struct Refreshed {
    access_token: Secret,
    /// Missing when the old refresh token stays valid.
    refresh_token: Option<Secret>,
    /// Seconds.
    expires_in: i64,
    /// Seconds.
    refresh_token_expires_in: Option<i64>,
    /// Space-separated.
    scope: Option<String>,
}

/// The credentials file with the refreshed tokens in place, every other field and the
/// key order unchanged, written compactly as Claude Code writes it.
fn updated_credentials(
    text: &str,
    refreshed: &Refreshed,
    now: DateTime<Utc>,
) -> anyhow::Result<String> {
    let mut root: Value = serde_json::from_str(text)
        .map_err(|_| anyhow!("the Claude Code credentials file has an unexpected format"))?;
    let oauth: &mut Map<String, Value> = root
        .get_mut("claudeAiOauth")
        .and_then(Value::as_object_mut)
        .context("Claude Code is not signed in with a Claude account")?;
    let in_ms = |secs: i64| {
        now.timestamp_millis()
            .saturating_add(secs.saturating_mul(1000))
    };
    // Copies of the tokens, deliberately: the caller still needs the access token.
    oauth.insert(
        "accessToken".into(),
        refreshed.access_token.0.clone().into(),
    );
    if let Some(refresh_token) = &refreshed.refresh_token {
        oauth.insert("refreshToken".into(), refresh_token.0.clone().into());
    }
    oauth.insert("expiresAt".into(), in_ms(refreshed.expires_in).into());
    if let Some(secs) = refreshed.refresh_token_expires_in {
        oauth.insert("refreshTokenExpiresAt".into(), in_ms(secs).into());
    }
    if let Some(scope) = &refreshed.scope {
        let scopes: Vec<&str> = scope.split_whitespace().collect();
        oauth.insert("scopes".into(), scopes.into());
    }
    serde_json::to_string(&root).context("writing the credentials as JSON")
}

/// Replaces `path` in one step, so a reader never sees a half-written login.
fn write_atomically(path: &Path, text: &str) -> anyhow::Result<()> {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".ai-notch-tmp");
    let temp = path.with_file_name(name);
    fs::write(&temp, text).with_context(|| format!("writing {}", temp.display()))?;
    if let Err(err) = fs::rename(&temp, path) {
        // Best effort; a leftover temp file is overwritten next time anyway.
        let _ = fs::remove_file(&temp);
        return Err(err).with_context(|| format!("replacing {}", path.display()));
    }
    Ok(())
}

/// One limit: an entry of `limits`, or the older `five_hour` and `seven_day` objects,
/// which call the percentage `utilization`.
#[derive(Debug, Deserialize)]
struct Limit {
    kind: Option<String>,
    #[serde(alias = "utilization")]
    percent: Option<f32>,
    resets_at: Option<String>,
}

/// Reads the 5-hour and weekly limits from an endpoint response. Every field is
/// optional and entries that do not parse are skipped, since the format is unofficial.
fn parse_usage(body: &str) -> anyhow::Result<Usage> {
    let response: Value = serde_json::from_str(body).context("the usage response is not JSON")?;
    let limit = |kind: &str, older: &str| {
        let listed = response["limits"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|entry| Limit::deserialize(entry).ok())
            .find(|limit| limit.kind.as_deref() == Some(kind));
        listed.or_else(|| Limit::deserialize(&response[older]).ok())
    };
    let session = limit("session", "five_hour").filter(|limit| limit.percent.is_some());
    let weekly = limit("weekly_all", "seven_day").filter(|limit| limit.percent.is_some());
    if session.is_none() && weekly.is_none() {
        bail!("the usage response holds no limits");
    }
    Ok(Usage {
        used: session.as_ref().and_then(Limit::share),
        resets_at: session.as_ref().and_then(Limit::reset),
        weekly_used: weekly.as_ref().and_then(Limit::share),
        weekly_resets_at: weekly.as_ref().and_then(Limit::reset),
        ..Usage::default()
    })
}

impl Limit {
    /// The share used, from 0 to 1.
    fn share(&self) -> Option<f32> {
        self.percent.map(|percent| percent / 100.0)
    }

    fn reset(&self) -> Option<DateTime<Utc>> {
        let at = DateTime::parse_from_rfc3339(self.resets_at.as_deref()?).ok()?;
        Some(at.to_utc())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../tests/fixtures/usage/claude_oauth_usage.json");

    fn time(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .expect("valid test timestamp")
            .to_utc()
    }

    #[test]
    fn fixture_gives_the_session_and_weekly_limits() {
        let usage = parse_usage(FIXTURE).expect("fixture parses");
        assert_eq!(usage.used, Some(0.0));
        // The session window is not open, so it has no reset time.
        assert_eq!(usage.resets_at, None);
        assert_eq!(usage.weekly_used, Some(0.02));
        assert_eq!(
            usage.weekly_resets_at,
            Some(time("2026-10-29T23:00:00.114010Z"))
        );
        assert!(!usage.estimated);
    }

    #[test]
    fn older_fields_are_the_fallback() {
        let body = r#"{
            "five_hour": { "utilization": 41.0, "resets_at": "2026-10-27T05:00:00.5+00:00" },
            "seven_day": { "utilization": 7.5, "resets_at": null }
        }"#;
        let usage = parse_usage(body).expect("older format parses");
        assert_eq!(usage.used, Some(0.41));
        assert_eq!(usage.resets_at, Some(time("2026-10-27T05:00:00.5Z")));
        assert_eq!(usage.weekly_used, Some(0.075));
    }

    #[test]
    fn odd_entries_are_skipped_and_no_limits_is_an_error() {
        let body = r#"{"limits": [
            "not an object",
            { "kind": "session", "percent": "lots" },
            { "kind": "session", "percent": 12, "resets_at": "soon" }
        ]}"#;
        let usage = parse_usage(body).expect("one good entry is enough");
        assert_eq!(usage.used, Some(0.12));
        assert_eq!(usage.resets_at, None);
        assert_eq!(usage.weekly_used, None);

        assert!(parse_usage(r#"{"five_hour": null, "limits": []}"#).is_err());
        assert!(parse_usage("<html>").is_err());
    }

    const CREDENTIALS: &str = r#"{"claudeAiOauth":{"accessToken":"synthetic-old-access","refreshToken":"synthetic-old-refresh","expiresAt":1793000000000,"refreshTokenExpiresAt":1794000000000,"scopes":["user:inference","user:profile"],"subscriptionType":"pro","rateLimitTier":"default"},"mcpOAuth":{"example":{"accessToken":"synthetic-mcp"}}}"#;

    #[test]
    fn login_needs_a_refresh_shortly_before_it_expires() {
        let login = parse_login(CREDENTIALS).expect("fixture parses");
        assert_eq!(login.scopes, ["user:inference", "user:profile"]);
        let at = |ms| DateTime::from_timestamp_millis(ms).expect("valid time");
        assert!(!login.expires_by(at(1_793_000_000_000 - 61_000)));
        assert!(login.expires_by(at(1_793_000_000_000 - 60_000)));
        assert!(login.expires_by(at(1_793_000_000_000)));
    }

    #[test]
    fn refresh_replaces_only_the_token_fields() {
        let refreshed: Refreshed = serde_json::from_str(
            r#"{"token_type":"Bearer","access_token":"synthetic-new-access","refresh_token":"synthetic-new-refresh","expires_in":28800,"refresh_token_expires_in":2592000,"scope":"user:inference user:profile user:sessions:claude_code"}"#,
        )
        .expect("reply parses");
        let now = DateTime::from_timestamp_millis(1_793_100_000_000).expect("valid time");

        let text = updated_credentials(CREDENTIALS, &refreshed, now).expect("credentials update");

        assert!(
            !text.contains('\n'),
            "written compactly like Claude Code: {text}"
        );
        let root: Value = serde_json::from_str(&text).expect("written file is JSON");
        let oauth = &root["claudeAiOauth"];
        assert_eq!(oauth["accessToken"], "synthetic-new-access");
        assert_eq!(oauth["refreshToken"], "synthetic-new-refresh");
        assert_eq!(oauth["expiresAt"], 1_793_100_000_000_i64 + 28_800_000);
        assert_eq!(
            oauth["refreshTokenExpiresAt"],
            1_793_100_000_000_i64 + 2_592_000_000
        );
        assert_eq!(oauth["scopes"][2], "user:sessions:claude_code");
        assert_eq!(oauth["subscriptionType"], "pro");
        assert_eq!(oauth["rateLimitTier"], "default");
        assert_eq!(root["mcpOAuth"]["example"]["accessToken"], "synthetic-mcp");
        let keys: Vec<&String> = oauth.as_object().expect("object").keys().collect();
        assert_eq!(keys[0], "accessToken", "key order is kept");
    }

    #[test]
    fn refresh_without_a_new_refresh_token_keeps_the_old_one() {
        let refreshed: Refreshed =
            serde_json::from_str(r#"{"access_token":"synthetic-new-access","expires_in":3600}"#)
                .expect("reply parses");
        let now = DateTime::from_timestamp_millis(1_793_100_000_000).expect("valid time");
        let text = updated_credentials(CREDENTIALS, &refreshed, now).expect("credentials update");
        let root: Value = serde_json::from_str(&text).expect("written file is JSON");
        assert_eq!(
            root["claudeAiOauth"]["refreshToken"],
            "synthetic-old-refresh"
        );
        assert_eq!(
            root["claudeAiOauth"]["refreshTokenExpiresAt"],
            1_794_000_000_000_i64
        );
    }

    #[test]
    fn written_login_replaces_the_file() {
        let dir = env::temp_dir().join(format!("ai-notch-credentials-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp dir is writable");
        let path = dir.join(".credentials.json");
        fs::write(&path, CREDENTIALS).expect("temp file is writable");
        write_atomically(&path, "{}").expect("file is replaced");
        assert_eq!(fs::read_to_string(&path).expect("file exists"), "{}");
        assert_eq!(
            fs::read_dir(&dir).expect("dir lists").count(),
            1,
            "no temp file left"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn tokens_never_show_in_debug_or_errors() {
        let login = parse_login(CREDENTIALS).expect("fixture parses");
        let shown = format!("{login:?}");
        assert!(!shown.contains("synthetic"), "{shown}");

        for file in [
            r#"{"claudeAiOauth": "synthetic-secret"}"#,
            r#"{"claudeAiOauth": {"accessToken": "synthetic-secret", "expiresAt": "synthetic-secret"}}"#,
            r#"{"claudeAiOauth": {"accessToken": ["synthetic-secret"]}}"#,
        ] {
            let err = parse_login(file).expect_err("malformed credentials");
            assert!(!format!("{err:?}").contains("synthetic-secret"), "{err:?}");
        }
        assert!(parse_login("{}").is_err());
    }
}
