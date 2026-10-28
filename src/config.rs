//! User settings, read from `%APPDATA%\notchling\config.toml`.

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::usage::Provider;
use crate::window::Edge;

/// Shortest allowed wait for a click on a permission request.
const MIN_HOOK_TIMEOUT_SECS: u64 = 5;

/// Longest allowed wait. Claude Code kills command hooks after 600 s by default, and
/// the installed hook entry gets [`HOOK_ENTRY_MARGIN_SECS`] on top of this.
const MAX_HOOK_TIMEOUT_SECS: u64 = 580;

/// Extra seconds the Claude Code hook entry allows beyond our own wait, so the notch
/// always gives up first and Claude Code never kills the hook mid-reply.
pub const HOOK_ENTRY_MARGIN_SECS: u64 = 10;

/// Settings with their defaults; every field is optional in the file.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
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
    /// The providers whose usage is read and offered in the menu. The others are never
    /// read, so their CLIs and logs are left alone.
    providers: Vec<Provider>,
    /// Usage percentages that raise an alert, each once per usage window.
    usage_alerts: Vec<u32>,
    /// Windows' device name of the display the notch sits on; the primary display
    /// when missing or disconnected.
    #[serde(skip_serializing_if = "Option::is_none")]
    monitor: Option<String>,
    /// A key from when exact Claude usage could be turned off. It is accepted so older
    /// files still load, and never written again.
    #[serde(skip_serializing)]
    claude_account: Option<bool>,
    /// Whether Ctrl+Alt+Y and Ctrl+Alt+N answer the permission request on the notch.
    /// Off by default: a global key that allows commands is opt-in.
    request_shortcuts: bool,
    /// Whether the idle notch tucks into a thin sliver on the left and right edges until
    /// the pointer reaches it.
    tuck: bool,
    /// Whether the notch shows CPU load, and RAM in its panel.
    show_cpu: bool,
    /// Whether the notch shows GPU load, and VRAM in its panel.
    show_gpu: bool,
    /// The screen edge the notch sticks to; set by dragging it.
    edge: Option<Edge>,
    /// Where along that edge the notch sits, from 0 to 1: left to right on the top and
    /// bottom edges, top to bottom on the side ones; set by dragging it.
    position: f32,
    /// A key from before the notch stuck to any edge, where 1 meant the bottom one. It
    /// is read once to pick the edge and never written again.
    #[serde(skip_serializing)]
    position_y: Option<f32>,
    /// Whether the notch offers to add its Claude Code hooks when they are missing or
    /// name another exe. Turned off when the user says no.
    offer_hooks: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            hook_timeout_secs: 120,
            // The Gemini CLI's daily request limit for a free Google login.
            gemini_daily_requests: 1000,
            provider: Provider::Claude,
            providers: Provider::ALL.to_vec(),
            usage_alerts: vec![80, 95],
            monitor: None,
            claude_account: None,
            request_shortcuts: false,
            tuck: false,
            show_cpu: true,
            show_gpu: true,
            edge: None,
            position: 0.5,
            position_y: None,
            offer_hooks: true,
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

    pub fn set_provider(&mut self, provider: Provider) {
        self.provider = provider;
    }

    /// Whether `provider`'s usage is read and offered in the menu.
    #[must_use]
    pub fn tracks(&self, provider: Provider) -> bool {
        self.providers.contains(&provider)
    }

    /// The tracked providers, in menu order.
    #[must_use]
    pub fn providers(&self) -> Vec<Provider> {
        Provider::ALL
            .into_iter()
            .filter(|&provider| self.tracks(provider))
            .collect()
    }

    pub fn set_providers(&mut self, providers: Vec<Provider>) {
        self.providers = providers;
    }

    /// The provider the notch shows: the chosen one while it is tracked, else the next
    /// tracked one in menu order, or `None` when no provider is tracked.
    #[must_use]
    pub fn shown_provider(&self) -> Option<Provider> {
        let chosen = Provider::ALL
            .iter()
            .position(|&provider| provider == self.provider)
            .expect("Provider::ALL lists every provider");
        Provider::ALL
            .into_iter()
            .cycle()
            .skip(chosen)
            .take(Provider::ALL.len())
            .find(|&provider| self.tracks(provider))
    }

    /// Usage percentages that raise an alert.
    #[must_use]
    pub fn usage_alerts(&self) -> &[u32] {
        &self.usage_alerts
    }

    pub fn set_usage_alerts(&mut self, percents: Vec<u32>) {
        self.usage_alerts = percents;
    }

    pub fn set_hook_timeout_secs(&mut self, secs: u64) {
        self.hook_timeout_secs = secs;
    }

    pub fn set_gemini_daily_requests(&mut self, requests: u32) {
        self.gemini_daily_requests = requests;
    }

    /// Windows' device name of the display the notch sits on, if one was chosen.
    #[must_use]
    pub fn monitor(&self) -> Option<&str> {
        self.monitor.as_deref()
    }

    pub fn set_monitor(&mut self, device: Option<String>) {
        self.monitor = device;
    }

    /// Whether Ctrl+Alt+Y and Ctrl+Alt+N answer the permission request on the notch.
    #[must_use]
    pub fn request_shortcuts(&self) -> bool {
        self.request_shortcuts
    }

    pub fn set_request_shortcuts(&mut self, enabled: bool) {
        self.request_shortcuts = enabled;
    }

    /// Whether the notch offers to add its Claude Code hooks when they are missing.
    #[must_use]
    pub fn offer_hooks(&self) -> bool {
        self.offer_hooks
    }

    pub fn set_offer_hooks(&mut self, offered: bool) {
        self.offer_hooks = offered;
    }

    /// Whether the idle notch tucks into the left and right edges until pointed at.
    #[must_use]
    pub fn tuck(&self) -> bool {
        self.tuck
    }

    pub fn set_tuck(&mut self, enabled: bool) {
        self.tuck = enabled;
    }

    /// Whether the notch shows CPU load and RAM.
    #[must_use]
    pub fn show_cpu(&self) -> bool {
        self.show_cpu
    }

    pub fn set_show_cpu(&mut self, shown: bool) {
        self.show_cpu = shown;
    }

    /// Whether the notch shows GPU load and VRAM.
    #[must_use]
    pub fn show_gpu(&self) -> bool {
        self.show_gpu
    }

    pub fn set_show_gpu(&mut self, shown: bool) {
        self.show_gpu = shown;
    }

    /// The screen edge the notch sticks to.
    #[must_use]
    pub fn edge(&self) -> Edge {
        self.edge.unwrap_or(match self.position_y {
            Some(position_y) if position_y > 0.5 => Edge::Bottom,
            _ => Edge::Top,
        })
    }

    pub fn set_edge(&mut self, edge: Edge) {
        self.edge = Some(edge);
        self.position_y = None;
    }

    /// Where along its edge the notch sits, from 0 to 1.
    #[must_use]
    pub fn position(&self) -> f32 {
        fraction_or(self.position, 0.5)
    }

    pub fn set_position(&mut self, position: f32) {
        self.position = position;
    }

    /// Changes the config file and keeps every other setting, returning the new
    /// config. Comments in the file are not kept.
    ///
    /// # Errors
    ///
    /// Fails when `APPDATA` is not set, when the file cannot be written, and when the
    /// file is invalid, so a typo is never overwritten with defaults.
    pub fn update(change: impl FnOnce(&mut Self)) -> anyhow::Result<Self> {
        let path = config_path().context("APPDATA is not set")?;
        update_file(&path, change)
    }
}

/// `value` kept in 0..=1, or `fallback` when it is not a number.
fn fraction_or(value: f32, fallback: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        fallback
    }
}

fn update_file(path: &Path, change: impl FnOnce(&mut Config)) -> anyhow::Result<Config> {
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
    fs::write(path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(config)
}

/// `%APPDATA%\notchling\config.toml`, or `None` when `APPDATA` is not set.
///
/// The first time, the settings saved before the rename, in `%APPDATA%\ai-notch`, are
/// copied there, so nothing is lost.
#[must_use]
pub fn config_path() -> Option<PathBuf> {
    let appdata = PathBuf::from(env::var_os("APPDATA")?);
    let path = appdata.join("notchling").join("config.toml");
    let old = appdata.join("ai-notch").join("config.toml");
    if !path.exists() && old.exists() {
        // On failure the defaults apply, as with any unreadable file.
        let _ = fs::create_dir_all(appdata.join("notchling")).and_then(|()| fs::copy(&old, &path));
    }
    Some(path)
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
        assert_eq!(config.shown_provider(), Some(Provider::ChatGpt));
        assert_eq!(Config::default().shown_provider(), Some(Provider::Claude));
        assert!(Config::parse("provider = \"copilot\"").is_err());
        assert_eq!(Config::default().usage_alerts(), [80, 95]);
        let alerts = Config::parse("usage_alerts = [50, 90]").expect("valid TOML");
        assert_eq!(alerts.usage_alerts(), [50, 90]);
    }

    #[test]
    fn an_untracked_choice_passes_to_the_next_tracked_provider() {
        let shown = |text: &str| Config::parse(text).expect("valid TOML").shown_provider();
        assert_eq!(
            shown(""),
            Some(Provider::Claude),
            "every provider by default"
        );
        assert_eq!(shown("provider = \"gemini\""), Some(Provider::Gemini));
        let cases = [
            (
                "claude",
                "[\"gemini\", \"chatgpt\"]",
                Some(Provider::ChatGpt),
            ),
            (
                "chatgpt",
                "[\"claude\", \"gemini\"]",
                Some(Provider::Gemini),
            ),
            (
                "gemini",
                "[\"claude\", \"chatgpt\"]",
                Some(Provider::Claude),
            ),
            ("claude", "[]", None),
        ];
        for (provider, providers, want) in cases {
            let text = format!("provider = \"{provider}\"\nproviders = {providers}");
            assert_eq!(shown(&text), want, "{text}");
        }
        let config = Config::parse("providers = [\"gemini\", \"claude\"]").expect("valid TOML");
        assert_eq!(config.providers(), [Provider::Claude, Provider::Gemini]);
    }

    #[test]
    fn position_defaults_to_the_center_and_stays_on_the_edge() {
        assert_eq!(Config::default().position(), 0.5);
        let read = |text: &str| Config::parse(text).expect("valid TOML").position();
        assert_eq!(read("position = 0.2"), 0.2);
        assert_eq!(read("position = -3.0"), 0.0);
        assert_eq!(read("position = 7.5"), 1.0);
        assert_eq!(read("position = nan"), 0.5);
    }

    #[test]
    fn edge_defaults_to_the_top_and_old_files_still_load() {
        let edge = |text: &str| Config::parse(text).expect("valid TOML").edge();
        assert_eq!(edge(""), Edge::Top);
        assert_eq!(edge("edge = \"right\""), Edge::Right);
        assert_eq!(edge("position_y = 1.0"), Edge::Bottom, "the older key");
        assert_eq!(edge("position_y = 0.0"), Edge::Top);
        assert!(Config::parse("edge = \"middle\"").is_err());

        let mut config = Config::parse("position_y = 1.0").expect("valid TOML");
        config.set_edge(Edge::Left);
        let text = toml::to_string(&config).expect("config serializes");
        assert!(!text.contains("position_y"), "{text}");
        assert_eq!(Config::parse(&text).map(|c| c.edge()), Ok(Edge::Left));
    }

    #[test]
    fn monitor_and_toggles_round_trip() {
        let mut config = Config::default();
        assert_eq!(config.monitor(), None);
        assert!(
            !config.request_shortcuts(),
            "keys that allow commands are opt-in"
        );
        config.set_monitor(Some(r"\\.\DISPLAY2".to_owned()));
        config.set_request_shortcuts(true);
        config.set_tuck(true);
        assert!(config.offer_hooks(), "missing hooks are offered by default");
        config.set_offer_hooks(false);
        assert!(
            config.show_cpu() && config.show_gpu(),
            "both show by default"
        );
        config.set_show_gpu(false);
        config.set_providers(vec![Provider::Gemini]);
        let text = toml::to_string(&config).expect("config serializes");
        assert_eq!(Config::parse(&text), Ok(config));
        assert!(
            !toml::to_string(&Config::default())
                .expect("serializes")
                .contains("monitor")
        );
    }

    #[test]
    fn update_keeps_other_settings_and_never_overwrites_a_broken_file() {
        let dir = env::temp_dir().join(format!("notchling-config-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("notchling").join("config.toml");
        let pick_gemini = |config: &mut Config| config.set_provider(Provider::Gemini);
        let read = |path: &Path| {
            let text = fs::read_to_string(path).expect("config file exists");
            Config::parse(&text).expect("written config is valid")
        };

        update_file(&path, pick_gemini).expect("a missing file is created");
        assert_eq!(read(&path).shown_provider(), Some(Provider::Gemini));

        fs::write(&path, "hook_timeout_secs = 45\n").expect("temp file is writable");
        update_file(&path, pick_gemini).expect("a valid file is updated");
        assert_eq!(read(&path).hook_timeout(), Duration::from_secs(45));
        assert_eq!(read(&path).shown_provider(), Some(Provider::Gemini));

        fs::write(&path, "hook_timeout = 45\n").expect("temp file is writable");
        assert!(update_file(&path, pick_gemini).is_err());
        let text = fs::read_to_string(&path).expect("config file exists");
        assert_eq!(text, "hook_timeout = 45\n", "a broken file stays untouched");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn the_old_claude_account_key_still_loads_and_is_dropped() {
        let config = Config::parse("claude_account = false").expect("older files still load");
        let text = toml::to_string(&config).expect("config serializes");
        assert!(!text.contains("claude_account"), "{text}");
    }

    #[test]
    fn typos_and_bad_types_are_errors() {
        assert!(Config::parse("hook_timeout = 45").is_err());
        assert!(Config::parse("hook_timeout_secs = \"45\"").is_err());
    }
}
