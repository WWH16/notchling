//! The settings window: shows the config file's values and saves the edits.

use std::cell::RefCell;

use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::config::Config;
use crate::ui::Settings;
use crate::usage::Provider;
use crate::window;

/// Opens the settings window filled from the config file, or brings it back if it is
/// already open. `saved` runs with the new config after each successful save.
///
/// # Errors
///
/// Fails when Slint cannot create or show the window.
pub fn open(
    slot: &RefCell<Option<Settings>>,
    saved: impl Fn(&Config) + 'static,
) -> anyhow::Result<()> {
    let settings = {
        let mut slot = slot.borrow_mut();
        match slot.as_ref() {
            Some(settings) => settings.clone_strong(),
            None => slot.insert(Settings::new()?).clone_strong(),
        }
    };

    let config = Config::load();
    let monitors = window::monitors();
    let labels: Vec<SharedString> = monitors.iter().map(|m| m.label().into()).collect();
    let selected = monitors
        .iter()
        .position(|monitor| Some(monitor.device.as_str()) == config.monitor())
        .or_else(|| monitors.iter().position(|monitor| monitor.primary))
        .unwrap_or(0);
    let alerts = config.usage_alerts();
    let alert = |index: usize| alerts.get(index).map_or(0, |&percent| int(percent));
    settings.set_monitors(ModelRc::new(VecModel::from(labels)));
    settings.set_monitor_index(int(selected));
    settings.set_first_alert(alert(0));
    settings.set_second_alert(alert(1));
    settings.set_permission_wait(int(config.hook_timeout().as_secs()));
    settings.set_gemini_limit(int(config.gemini_daily_requests()));
    settings.set_claude_account(config.claude_account());
    settings.set_request_shortcuts(config.request_shortcuts());
    settings.set_tuck(config.tuck());
    settings.set_show_cpu(config.show_cpu());
    settings.set_show_gpu(config.show_gpu());
    settings.set_track_claude(config.tracks(Provider::Claude));
    settings.set_track_chatgpt(config.tracks(Provider::ChatGpt));
    settings.set_track_gemini(config.tracks(Provider::Gemini));
    settings.set_error(SharedString::new());

    let weak = settings.as_weak();
    settings.on_cancel(move || {
        if let Some(settings) = weak.upgrade() {
            // Only fails when the window is already gone.
            let _ = settings.hide();
        }
    });

    let weak = settings.as_weak();
    settings.on_save(move || {
        let Some(settings) = weak.upgrade() else {
            return;
        };
        let monitor = usize::try_from(settings.get_monitor_index())
            .ok()
            .and_then(|index| monitors.get(index))
            .map(|monitor| monitor.device.clone());
        let alerts: Vec<u32> = [settings.get_first_alert(), settings.get_second_alert()]
            .into_iter()
            .filter_map(|percent| u32::try_from(percent).ok())
            .filter(|&percent| percent > 0)
            .collect();
        let wait = u64::try_from(settings.get_permission_wait()).unwrap_or(0);
        let gemini = u32::try_from(settings.get_gemini_limit()).unwrap_or(1);
        let account = settings.get_claude_account();
        let shortcuts = settings.get_request_shortcuts();
        let tuck = settings.get_tuck();
        let (show_cpu, show_gpu) = (settings.get_show_cpu(), settings.get_show_gpu());
        let tracked = [
            settings.get_track_claude(),
            settings.get_track_chatgpt(),
            settings.get_track_gemini(),
        ];
        let providers: Vec<Provider> = Provider::ALL
            .into_iter()
            .zip(tracked)
            .filter_map(|(provider, tracked)| tracked.then_some(provider))
            .collect();
        let updated = Config::update(|config| {
            config.set_monitor(monitor);
            config.set_usage_alerts(alerts);
            config.set_hook_timeout_secs(wait);
            config.set_gemini_daily_requests(gemini);
            config.set_claude_account(account);
            config.set_request_shortcuts(shortcuts);
            config.set_tuck(tuck);
            config.set_show_cpu(show_cpu);
            config.set_show_gpu(show_gpu);
            config.set_providers(providers);
        });
        match updated {
            Ok(config) => {
                let _ = settings.hide();
                saved(&config);
            }
            Err(err) => settings.set_error(format!("Not saved: {err:#}").into()),
        }
    });

    settings.show()?;
    Ok(())
}

/// A count or setting for a Slint `int`; none of them come near `i32::MAX`.
fn int(value: impl TryInto<i32>) -> i32 {
    value.try_into().unwrap_or(i32::MAX)
}
