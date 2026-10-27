//! Claude's exact usage limits from the account endpoint behind Claude Code's `/usage`
//! screen, signed in with Claude Code's own login.
//!
//! The endpoint is unofficial. The login is only read: ai-notch never refreshes it, so
//! it cannot sign Claude Code out. While the login is expired there is no reading, until
//! Claude Code refreshes it.

use std::env;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;
use ureq::Agent;
use ureq::tls::{RootCerts, TlsConfig, TlsProvider};

use super::Usage;

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";

/// Beta flag the endpoint requires for OAuth logins.
const OAUTH_BETA: &str = "oauth-2025-04-20";

/// A slower request is dropped, so the poll thread moves on to the next reading.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Reads Claude's usage limits from the account endpoint.
#[derive(Debug)]
pub struct ClaudeAccount {
    credentials: PathBuf,
    agent: Agent,
}

impl ClaudeAccount {
    /// Uses the login of the current Windows user's Claude Code.
    ///
    /// # Errors
    ///
    /// Fails when `USERPROFILE` is not set.
    pub fn try_new() -> anyhow::Result<Self> {
        let home = env::var_os("USERPROFILE").context("USERPROFILE is not set")?;
        // Windows' own TLS and certificate store, so corporate proxies' roots work too.
        let tls = TlsConfig::builder()
            .provider(TlsProvider::NativeTls)
            .root_certs(RootCerts::PlatformVerifier)
            .build();
        let agent = Agent::config_builder()
            .tls_config(tls)
            .https_only(true)
            .timeout_global(Some(REQUEST_TIMEOUT))
            .user_agent(concat!("ai-notch/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        Ok(Self {
            credentials: Path::new(&home).join(".claude").join(".credentials.json"),
            agent,
        })
    }

    /// Asks the account endpoint for the current limits.
    ///
    /// # Errors
    ///
    /// Fails when Claude Code is not signed in or its login expired, when the request
    /// fails, or when the response holds no limits.
    pub fn fetch(&self) -> anyhow::Result<Usage> {
        let token = read_token(&self.credentials, Utc::now())?;
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
}

/// Claude Code's OAuth access token. `Debug` hides it, so it never reaches a log.
#[derive(Deserialize)]
#[serde(transparent)]
struct AccessToken(String);

impl fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AccessToken(..)")
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Credentials {
    claude_ai_oauth: Option<OAuth>,
}

/// The only login fields read; the refresh token is never touched.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OAuth {
    access_token: AccessToken,
    /// Unix time in milliseconds.
    expires_at: Option<i64>,
}

fn read_token(path: &Path, now: DateTime<Utc>) -> anyhow::Result<AccessToken> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    parse_token(&text, now)
}

/// The access token in a credentials file, if it is still valid at `now`.
fn parse_token(text: &str, now: DateTime<Utc>) -> anyhow::Result<AccessToken> {
    // serde_json errors can quote values from the file, so they are dropped.
    let credentials: Credentials = serde_json::from_str(text)
        .map_err(|_| anyhow!("the Claude Code credentials file has an unexpected format"))?;
    let oauth = credentials
        .claude_ai_oauth
        .context("Claude Code is not signed in with a Claude account")?;
    if oauth
        .expires_at
        .is_some_and(|expires| expires <= now.timestamp_millis())
    {
        bail!("the Claude Code login expired; waiting for Claude Code to refresh it");
    }
    Ok(oauth.access_token)
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
    let weekly = limit("weekly_all", "seven_day").and_then(|limit| limit.percent);
    if session.is_none() && weekly.is_none() {
        bail!("the usage response holds no limits");
    }
    Ok(Usage {
        used: session
            .as_ref()
            .and_then(|limit| limit.percent)
            .map(|percent| percent / 100.0),
        resets_at: session
            .and_then(|limit| limit.resets_at)
            .and_then(|at| DateTime::parse_from_rfc3339(&at).ok())
            .map(|at| at.to_utc()),
        weekly_used: weekly.map(|percent| percent / 100.0),
        ..Usage::default()
    })
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

    #[test]
    fn token_is_read_until_it_expires() {
        let file = r#"{"claudeAiOauth": {
            "accessToken": "synthetic-secret",
            "refreshToken": "synthetic-refresh",
            "expiresAt": 1793000000000,
            "subscriptionType": "pro"
        }}"#;
        let before = DateTime::from_timestamp_millis(1_792_999_999_999).expect("valid time");
        let token = parse_token(file, before).expect("token is still valid");
        assert_eq!(token.0, "synthetic-secret");

        let at = DateTime::from_timestamp_millis(1_793_000_000_000).expect("valid time");
        let err = parse_token(file, at).expect_err("token expired");
        assert!(err.to_string().contains("expired"), "{err}");
    }

    #[test]
    fn token_never_shows_in_debug_or_errors() {
        let now = time("2026-10-27T00:00:00Z");
        let token = parse_token(
            r#"{"claudeAiOauth": {"accessToken": "synthetic-secret"}}"#,
            now,
        )
        .expect("a token without expiry is valid");
        assert_eq!(format!("{token:?}"), "AccessToken(..)");

        for file in [
            r#"{"claudeAiOauth": "synthetic-secret"}"#,
            r#"{"claudeAiOauth": {"accessToken": "synthetic-secret", "expiresAt": "synthetic-secret"}}"#,
            r#"{"claudeAiOauth": {"accessToken": ["synthetic-secret"]}}"#,
        ] {
            let err = parse_token(file, now).expect_err("malformed credentials");
            assert!(!format!("{err:?}").contains("synthetic-secret"), "{err:?}");
        }
        assert!(parse_token("{}", now).is_err());
    }
}
