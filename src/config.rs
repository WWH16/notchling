//! User settings, read from `%APPDATA%\ai-notch\config.toml`.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

/// Shortest allowed wait for a click on a permission request.
const MIN_HOOK_TIMEOUT_SECS: u64 = 5;

/// Longest allowed wait. Claude Code kills command hooks after 600 s by default, and
/// the installed hook entry gets [`HOOK_ENTRY_MARGIN_SECS`] on top of this.
const MAX_HOOK_TIMEOUT_SECS: u64 = 580;

/// Extra seconds the Claude Code hook entry allows beyond our own wait, so the notch
/// always gives up first and Claude Code never kills the hook mid-reply.
pub const HOOK_ENTRY_MARGIN_SECS: u64 = 10;

/// Settings with their defaults; every field is optional in the file.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Seconds a permission request waits for a click before Claude Code shows its
    /// own dialog.
    hook_timeout_secs: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            hook_timeout_secs: 120,
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
    fn typos_and_bad_types_are_errors() {
        assert!(Config::parse("hook_timeout = 45").is_err());
        assert!(Config::parse("hook_timeout_secs = \"45\"").is_err());
    }
}
