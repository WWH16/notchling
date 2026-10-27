//! User settings, read from `%APPDATA%\ai-notch\config.toml`.

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::usage::Provider;

/// Shortest allowed wait for a click on a permission request.
const MIN_HOOK_TIMEOUT_SECS: u64 = 5;

/// Longest allowed wait. Claude Code kills command hooks after 600 s by default, and
/// the installed hook entry gets [`HOOK_ENTRY_MARGIN_SECS`] on top of this.
const MAX_HOOK_TIMEOUT_SECS: u64 = 580;

/// Extra seconds the Claude Code hook entry allows beyond our own wait, so the notch
/// always gives up first and Claude Code never kills the hook mid-reply.
pub const HOOK_ENTRY_MARGIN_SECS: u64 = 10;

/// Settings with their defaults; every field is optional in the file.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Seconds a permission request waits for a click before Claude Code shows its
    /// own dialog.
    hook_timeout_secs: u64,
    /// Gemini CLI requests allowed per day. The CLI logs no quota, so this is the
    /// limit the Gemini share is estimated against.
    gemini_daily_requests: u32,
    /// The provider whose usage the notch shows.
    provider: Provider,
    /// Usage percentages that raise an alert, each once per usage window.
    usage_alerts: Vec<u32>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            hook_timeout_secs: 120,
            // The Gemini CLI's daily request limit for a free Google login.
            gemini_daily_requests: 1000,
            provider: Provider::Claude,
            usage_alerts: vec![80, 95],
        }
    }
}

impl Config {
    /// Reads the config file. A missing or invalid file gives the defaults, so a typo
    /// never stops the notch or the hook from working.
    #[must_use]
    pub fn load() -> Self {
        config_path()
            .and_then(|path| fs::read_to_string(path).ok())
            .and_then(|text| Self::parse(&text).ok())
            .unwrap_or_default()
    }

    /// Parses config file contents.
    ///
    /// # Errors
    ///
    /// Fails on invalid TOML, unknown keys, or wrongly typed values.
    pub fn parse(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }

    /// How long a permission request waits for a click, clamped to a safe range.
    #[must_use]
    pub fn hook_timeout(&self) -> Duration {
        let secs = self
            .hook_timeout_secs
            .clamp(MIN_HOOK_TIMEOUT_SECS, MAX_HOOK_TIMEOUT_SECS);
        Duration::from_secs(secs)
    }

    /// Gemini CLI requests allowed per day, at least 1.
    #[must_use]
    pub fn gemini_daily_requests(&self) -> u32 {
        self.gemini_daily_requests.max(1)
    }

    /// The provider whose usage the notch shows.
    #[must_use]
    pub fn provider(&self) -> Provider {
        self.provider
    }

    pub fn set_provider(&mut self, provider: Provider) {
        self.provider = provider;
    }

    /// Usage percentages that raise an alert.
    #[must_use]
    pub fn usage_alerts(&self) -> &[u32] {
        &self.usage_alerts
    }

    /// Changes the config file and keeps every other setting. Comments in the file are
    /// not kept.
    ///
    /// # Errors
    ///
    /// Fails when `APPDATA` is not set, when the file cannot be written, and when the
    /// file is invalid, so a typo is never overwritten with defaults.
    pub fn update(change: impl FnOnce(&mut Self)) -> anyhow::Result<()> {
        let path = config_path().context("APPDATA is not set")?;
        update_file(&path, change)
    }
}

fn update_file(path: &Path, change: impl FnOnce(&mut Config)) -> anyhow::Result<()> {
    let mut config = match fs::read_to_string(path) {
        Ok(text) => Config::parse(&text)
            .with_context(|| format!("{} has an error; fix it first", path.display()))?,
        Err(err) if err.kind() == io::ErrorKind::NotFound => Config::default(),
        Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
    };
    change(&mut config);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let text = toml::to_string(&config).context("writing the config as TOML")?;
    fs::write(path, text).with_context(|| format!("writing {}", path.display()))
}

/// `%APPDATA%\ai-notch\config.toml`, or `None` when `APPDATA` is not set.
#[must_use]
pub fn config_path() -> Option<PathBuf> {
    let appdata = env::var_os("APPDATA")?;
    Some(PathBuf::from(appdata).join("ai-notch").join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_gives_defaults() {
        let config = Config::parse("").expect("empty TOML is valid");
        assert_eq!(config, Config::default());
        assert_eq!(config.hook_timeout(), Duration::from_secs(120));
    }

    #[test]
    fn timeout_is_read_and_clamped() {
        let read = |text: &str| Config::parse(text).expect("valid TOML").hook_timeout();
        assert_eq!(read("hook_timeout_secs = 45"), Duration::from_secs(45));
        assert_eq!(read("hook_timeout_secs = 1"), Duration::from_secs(5));
        assert_eq!(read("hook_timeout_secs = 9999"), Duration::from_secs(580));
    }

    #[test]
    fn gemini_limit_and_provider_are_read() {
        let config =
            Config::parse("gemini_daily_requests = 0\nprovider = \"chatgpt\"").expect("valid TOML");
        assert_eq!(config.gemini_daily_requests(), 1);
        assert_eq!(config.provider(), Provider::ChatGpt);
        assert_eq!(Config::default().provider(), Provider::Claude);
        assert!(Config::parse("provider = \"copilot\"").is_err());
        assert_eq!(Config::default().usage_alerts(), [80, 95]);
        let alerts = Config::parse("usage_alerts = [50, 90]").expect("valid TOML");
        assert_eq!(alerts.usage_alerts(), [50, 90]);
    }

    #[test]
    fn update_keeps_other_settings_and_never_overwrites_a_broken_file() {
        let dir = env::temp_dir().join(format!("ai-notch-config-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("ai-notch").join("config.toml");
        let pick_gemini = |config: &mut Config| config.set_provider(Provider::Gemini);
        let read = |path: &Path| {
            let text = fs::read_to_string(path).expect("config file exists");
            Config::parse(&text).expect("written config is valid")
        };

        update_file(&path, pick_gemini).expect("a missing file is created");
        assert_eq!(read(&path).provider(), Provider::Gemini);

        fs::write(&path, "hook_timeout_secs = 45\n").expect("temp file is writable");
        update_file(&path, pick_gemini).expect("a valid file is updated");
        assert_eq!(read(&path).hook_timeout(), Duration::from_secs(45));
        assert_eq!(read(&path).provider(), Provider::Gemini);

        fs::write(&path, "hook_timeout = 45\n").expect("temp file is writable");
        assert!(update_file(&path, pick_gemini).is_err());
        let text = fs::read_to_string(&path).expect("config file exists");
        assert_eq!(text, "hook_timeout = 45\n", "a broken file stays untouched");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn typos_and_bad_types_are_errors() {
        assert!(Config::parse("hook_timeout = 45").is_err());
        assert!(Config::parse("hook_timeout_secs = \"45\"").is_err());
    }
}
