#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod awake;
mod config;
mod crash;
mod gpu;
mod hook;
mod http;
mod install;
mod pipe;
mod process;
mod protocol;
mod requests;
mod server;
mod status;
mod system;
mod usage;
mod window;

#[allow(missing_debug_implementations, reason = "slint-generated types")]
mod ui {
    slint::include_modules!();
}

use std::collections::HashMap;
use std::env;
use std::mem;
use std::rc::Rc;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use awake::KeepAwake;
use chrono::Utc;
use config::Config;
use gpu::GpuLoad;
use pipe::Listener;
use protocol::{Decision, Event, Origin};
use requests::{RequestId, Requests};
use server::Update;
use slint::{ComponentHandle, Timer, TimerMode, Weak};
use status::Health;
use system::SystemLoad;
use ui::{AlertKind, Choice, Notch};
use usage::{Claude, Codex, Gemini, Provider, Reading, Usage};
use window::{MenuChoice, MessageKind, NotchWindow};

/// One frame at 60 Hz; how often to check whether the native window exists yet.
const FRAME: Duration = Duration::from_millis(16);

/// About one second of frames before giving up on the native window.
const NATIVE_WINDOW_ATTEMPTS: u32 = 60;

/// How often the CPU, GPU and RAM readings refresh, matching Task Manager's default.
const STATS_INTERVAL: Duration = Duration::from_secs(1);

/// How long "task done" stays on the notch.
const DONE_ALERT: Duration = Duration::from_secs(5);

/// How long "needs your input" stays; longer, since it waits on the user.
const NEEDS_INPUT_ALERT: Duration = Duration::from_secs(15);

/// How often silent sessions are checked against the keep-awake safety window.
const AWAKE_CHECK: Duration = Duration::from_secs(60);

/// How often Claude Code's logs are checked; each check parses only appended lines.
/// Also how often Claude's reset countdown ticks, which shows whole minutes. The account
/// endpoint is asked less often; see `usage::Claude`.
const CLAUDE_INTERVAL: Duration = Duration::from_secs(30);

/// How often the Codex and Gemini CLI logs are checked. Their limits reset over days,
/// so a minute is fresh enough.
const CLI_LOG_INTERVAL: Duration = Duration::from_secs(60);

fn main() -> anyhow::Result<()> {
    match env::args().nth(1).as_deref() {
        Some("--hook") => {
            crash::install(crash::Report::NoteOnly);
            hook::run();
            return Ok(());
        }
        Some("--install-hooks") => {
            report_settings_change("added to", install::install_hooks());
            return Ok(());
        }
        Some("--uninstall-hooks") => {
            report_settings_change("removed from", install::uninstall_hooks());
            return Ok(());
        }
        _ => {}
    }

    crash::install(crash::Report::NoteAndDialog);

    // Claiming the pipe first also stops a second notch from starting.
    let listener = Listener::bind(&pipe::pipe_name()?)?;

    let config = Config::load();
    let usages = Arc::new(Mutex::new(Usages::new(config.provider())));

    let notch = Notch::new()?;
    window::place_top_center(notch.window(), notch.get_fixed_width())?;
    show_usage(&notch, config.provider(), None);
    notch.show()?;
    let status_switch = start_status(&notch, &usages);

    let menu_usages = Arc::clone(&usages);
    on_native_window(
        notch.as_weak(),
        NATIVE_WINDOW_ATTEMPTS,
        move |notch, native| {
            native.apply_tool_styles();
            let native = Rc::new(native);

            let menu_native = Rc::clone(&native);
            let weak = notch.as_weak();
            notch.on_menu_requested(move || {
                let shown = lock_usages(&menu_usages).shown;
                match menu_native.show_menu(shown) {
                    Some(MenuChoice::Quit) => {
                        // Only fails when the loop already stopped, which is what we want anyway.
                        let _ = slint::quit_event_loop();
                    }
                    Some(MenuChoice::Show(provider)) => {
                        if let Some(notch) = weak.upgrade() {
                            select_provider(&notch, &menu_usages, &status_switch, provider);
                        }
                    }
                    None => {}
                }
            });

            let weak = notch.as_weak();
            let clip = move || {
                let Some(notch) = weak.upgrade() else {
                    return;
                };
                native.clip_to(window::shape_rect(
                    notch.get_fixed_width(),
                    notch.get_shape_width(),
                    notch.get_shape_height(),
                    notch.window().scale_factor(),
                ));
            };
            clip();
            notch.on_shape_changed(clip);
        },
    );

    let _stats = start_stats(&notch);
    start_usage(&notch, &usages, &config);
    let _awake_check = serve_hooks(&notch, listener);

    slint::run_event_loop()?;
    Ok(())
}

/// Tells the user what `--install-hooks` or `--uninstall-hooks` did. A message box,
/// because the release exe has no console to print to.
fn report_settings_change(verb: &str, outcome: anyhow::Result<install::Outcome>) {
    match outcome {
        Ok(outcome) => {
            let backup = outcome.backup.map_or_else(
                || "There was no settings file before, so nothing was backed up.".to_owned(),
                |path| format!("The previous file was backed up to:\n{}", path.display()),
            );
            let text = format!(
                "The ai-notch hooks were {verb}\n{}\n\n{backup}\n\nClaude Code picks up the change automatically.",
                outcome.settings.display()
            );
            window::message_box("ai-notch", &text, MessageKind::Info);
        }
        Err(err) => {
            let text = format!("Nothing was changed.\n\n{err:#}");
            window::message_box("ai-notch", &text, MessageKind::Error);
        }
    }
}

/// Refreshes the CPU, GPU and RAM readings until the returned timer is dropped.
fn start_stats(notch: &Notch) -> Timer {
    let timer = Timer::default();
    let mut load = SystemLoad::new();
    // Without GPU counters the GPU and VRAM stats stay at their placeholders.
    let mut gpu = GpuLoad::open().ok();
    let weak = notch.as_weak();
    timer.start(TimerMode::Repeated, STATS_INTERVAL, move || {
        let Some(notch) = weak.upgrade() else {
            return;
        };
        let reading = load.sample();
        notch.set_cpu(reading.cpu);
        notch.set_ram(reading.ram_fraction());
        notch.set_ram_text(system::gigabytes(reading.ram_used_bytes).into());

        if let Some(gpu) = gpu.as_mut() {
            let reading = gpu.sample();
            notch.set_gpu(reading.utilization);
            if let Some(used) = reading.vram_used_bytes {
                let total = reading.vram_total_bytes.unwrap_or(0);
                notch.set_vram(system::fraction(used, total));
                notch.set_vram_text(system::gigabytes(used).into());
            }
        }
    });
    timer
}

/// The latest good usage reading of each provider, and which one the notch shows.
#[derive(Debug)]
struct Usages {
    shown: Provider,
    latest: HashMap<Provider, Usage>,
}

impl Usages {
    fn new(shown: Provider) -> Self {
        Self {
            shown,
            latest: HashMap::new(),
        }
    }
}

fn lock_usages(usages: &Mutex<Usages>) -> std::sync::MutexGuard<'_, Usages> {
    usages.lock().expect("usages lock poisoned")
}

/// Polls every provider's usage in the background and keeps the notch showing the
/// selected one. A provider whose source cannot start shows as "no data".
fn start_usage(notch: &Notch, usages: &Arc<Mutex<Usages>>, config: &Config) {
    let weak = notch.as_weak();
    let usages = Arc::clone(usages);
    let notify = move |reading: Reading| {
        let usages = Arc::clone(&usages);
        // Fails only once the event loop has quit, when nothing needs drawing.
        let _ = weak.upgrade_in_event_loop(move |notch| {
            let mut usages = lock_usages(&usages);
            // A failed read drops the old numbers instead of leaving them up as current.
            match reading.usage {
                Ok(usage) => usages.latest.insert(reading.provider, usage),
                Err(_) => usages.latest.remove(&reading.provider),
            };
            if reading.provider == usages.shown {
                show_usage(&notch, usages.shown, usages.latest.get(&usages.shown));
            }
        });
    };
    if let Ok(source) = Claude::try_new() {
        usage::spawn(source, CLAUDE_INTERVAL, notify.clone());
    }
    if let Ok(source) = Codex::try_new() {
        usage::spawn(source, CLI_LOG_INTERVAL, notify.clone());
    }
    if let Ok(source) = Gemini::try_new(config.gemini_daily_requests()) {
        usage::spawn(source, CLI_LOG_INTERVAL, notify);
    }
}

/// Checks the shown provider's status page in the background and shows incidents on
/// the notch. Returns where to send the provider whenever the selection changes.
fn start_status(notch: &Notch, usages: &Arc<Mutex<Usages>>) -> Sender<Provider> {
    let weak = notch.as_weak();
    let usages = Arc::clone(usages);
    let shown = lock_usages(&usages).shown;
    status::spawn(shown, move |provider, health| {
        let usages = Arc::clone(&usages);
        // Fails only once the event loop has quit, when nothing needs drawing.
        let _ = weak.upgrade_in_event_loop(move |notch| {
            // A status that arrives after a switch belongs to the old provider.
            if lock_usages(&usages).shown == provider {
                notch.set_status(health.map_or(ui::Health::Unknown, ui::Health::from));
            }
        });
    })
}

/// Shows `provider` on the notch from now on, and remembers the choice in the config.
fn select_provider(
    notch: &Notch,
    usages: &Mutex<Usages>,
    status_switch: &Sender<Provider>,
    provider: Provider,
) {
    let mut usages = lock_usages(usages);
    usages.shown = provider;
    show_usage(notch, provider, usages.latest.get(&provider));
    notch.set_status(ui::Health::Unknown);
    // Fails only if the status thread is gone, which leaves the status unknown.
    let _ = status_switch.send(provider);
    // On failure the choice only lasts until the notch restarts.
    let _ = Config::update(|config| config.set_provider(provider));
}

/// Puts a provider's usage on the notch, or "no data" when there is no good reading.
fn show_usage(notch: &Notch, provider: Provider, usage: Option<&Usage>) {
    notch.set_usage_provider(provider.into());
    let Some(usage) = usage else {
        notch.set_usage_short("-".into());
        notch.set_usage_tokens("".into());
        notch.set_usage_rate("".into());
        notch.set_usage_weekly("".into());
        notch.set_usage_reset("no data".into());
        notch.set_usage_estimated(false);
        return;
    };
    let tokens = usage.tokens.map(usage::compact);
    let short = match (usage.used, &tokens) {
        (Some(used), _) => format!("{:.0}%", used * 100.0),
        (None, Some(tokens)) => tokens.clone(),
        (None, None) => "-".to_owned(),
    };
    // Float to integer casts saturate; display only.
    let rate = usage
        .burn_rate
        .map(|rate| format!("{}/min", usage::compact(rate.round() as u64)));
    let reset = usage.resets_at.map_or_else(
        || "no window open".to_owned(),
        |at| format!("resets in {}", usage::countdown(at - Utc::now())),
    );
    notch.set_usage_short(short.into());
    notch.set_usage_tokens(
        tokens
            .map(|tokens| format!("{tokens} tokens"))
            .unwrap_or_default()
            .into(),
    );
    notch.set_usage_rate(rate.unwrap_or_default().into());
    let weekly = usage
        .weekly_used
        .map(|used| format!("week {:.0}%", used * 100.0));
    notch.set_usage_weekly(weekly.unwrap_or_default().into());
    notch.set_usage_reset(reset.into());
    notch.set_usage_estimated(usage.estimated);
}

/// Accepts hook connections and reacts to their events on the notch. Returns the
/// timer that releases keep-awake for silent sessions; it runs until dropped.
fn serve_hooks(notch: &Notch, listener: Listener) -> Timer {
    let requests = Arc::new(Mutex::new(Requests::default()));

    let weak = notch.as_weak();
    let decided = Arc::clone(&requests);
    notch.on_decide(move |id, choice| {
        let Some(notch) = weak.upgrade() else {
            return;
        };
        let mut requests = decided.lock().expect("requests lock poisoned");
        requests.decide(RequestId::from_ui(id), Decision::from(choice));
        show_requests(&notch, &requests);
    });

    let alert = Arc::new(Mutex::new(Alert::default()));
    let clicked = Arc::clone(&alert);
    let weak = notch.as_weak();
    notch.on_alert_clicked(move || {
        let ancestors = mem::take(&mut clicked.lock().expect("alert lock poisoned").ancestors);
        if let Some(notch) = weak.upgrade() {
            notch.set_alert_active(false);
        }
        process::focus_first_window(&ancestors);
    });

    // Only ever locked on the UI thread, which owns the Windows power request.
    let awake = Arc::new(Mutex::new(KeepAwake::default()));
    let awake_check = Timer::default();
    let expiring = Arc::clone(&awake);
    awake_check.start(TimerMode::Repeated, AWAKE_CHECK, move || {
        expiring
            .lock()
            .expect("keep-awake lock poisoned")
            .expire(Instant::now());
    });

    let weak = notch.as_weak();
    let shown = Arc::clone(&requests);
    server::spawn(listener, requests, move |update| {
        let requests = Arc::clone(&shown);
        let alert = Arc::clone(&alert);
        let awake = Arc::clone(&awake);
        // Fails only once the event loop has quit, when nothing needs drawing.
        let _ = weak.upgrade_in_event_loop(move |notch| match update {
            Update::Requests => {
                show_requests(&notch, &requests.lock().expect("requests lock poisoned"));
            }
            Update::Session(Event::TaskDone { origin }) => {
                lock_awake(&awake).stopped(&origin.session_id);
                show_alert(&notch, &alert, AlertKind::Done, origin, DONE_ALERT);
            }
            Update::Session(Event::NeedsInput { origin, .. }) => {
                lock_awake(&awake).stopped(&origin.session_id);
                show_alert(
                    &notch,
                    &alert,
                    AlertKind::NeedsInput,
                    origin,
                    NEEDS_INPUT_ALERT,
                );
            }
            Update::Session(Event::PromptSubmitted { origin }) => {
                lock_awake(&awake).started(origin.session_id, Instant::now());
            }
            // The server turns these into `Update::Requests`.
            Update::Session(Event::PermissionRequest { .. }) => {}
        });
    });
    awake_check
}

fn lock_awake(awake: &Mutex<KeepAwake>) -> std::sync::MutexGuard<'_, KeepAwake> {
    awake.lock().expect("keep-awake lock poisoned")
}

/// The alert on screen: which one it is, and where its session runs.
#[derive(Debug, Default)]
struct Alert {
    /// Bumped per alert, so an old alert's timer cannot hide a newer one.
    serial: u64,
    ancestors: Vec<u32>,
}

/// Shows an attention message for `duration`, or until it is clicked.
fn show_alert(
    notch: &Notch,
    alert: &Arc<Mutex<Alert>>,
    kind: AlertKind,
    origin: Origin,
    duration: Duration,
) {
    let serial = {
        let mut current = alert.lock().expect("alert lock poisoned");
        current.serial += 1;
        current.ancestors = origin.ancestors;
        current.serial
    };
    let label = match kind {
        AlertKind::Done => "Done",
        AlertKind::NeedsInput => "Needs your input",
    };
    let text = if origin.project.is_empty() {
        label.to_owned()
    } else {
        format!("{label} · {}", origin.project)
    };
    notch.set_alert_kind(kind);
    notch.set_alert_text(text.into());
    notch.set_alert_active(true);

    let weak = notch.as_weak();
    let alert = Arc::clone(alert);
    Timer::single_shot(duration, move || {
        let still_current = alert.lock().expect("alert lock poisoned").serial == serial;
        if let (true, Some(notch)) = (still_current, weak.upgrade()) {
            notch.set_alert_active(false);
        }
    });
}

/// Puts the front request on the notch, or hides the request card when none is left.
fn show_requests(notch: &Notch, requests: &Requests) {
    let Some(front) = requests.front() else {
        // The texts stay so the card can fade out with its content.
        notch.set_has_request(false);
        return;
    };
    notch.set_request_id(front.id.to_ui());
    notch.set_request_tool(front.tool.as_str().into());
    notch.set_request_project(front.project.as_str().into());
    notch.set_request_detail(front.detail.as_str().into());
    notch.set_request_more(i32::try_from(requests.behind_front()).unwrap_or(i32::MAX));
    notch.set_has_request(true);
}

impl From<usage::Provider> for ui::Provider {
    fn from(provider: usage::Provider) -> Self {
        match provider {
            usage::Provider::Claude => Self::Claude,
            usage::Provider::ChatGpt => Self::Chatgpt,
            usage::Provider::Gemini => Self::Gemini,
        }
    }
}

impl From<Health> for ui::Health {
    fn from(health: Health) -> Self {
        match health {
            Health::Operational => Self::Operational,
            Health::Degraded => Self::Degraded,
            Health::Down => Self::Down,
        }
    }
}

impl From<Choice> for Decision {
    fn from(choice: Choice) -> Self {
        match choice {
            Choice::Allow => Self::Allow,
            Choice::Deny => Self::Deny,
            Choice::AskInApp => Self::AskInApp,
        }
    }
}

/// Calls `ready` once Slint has created the native window, checking once per frame.
fn on_native_window(
    notch: Weak<Notch>,
    attempts_left: u32,
    ready: impl FnOnce(Notch, NotchWindow) + 'static,
) {
    Timer::single_shot(FRAME, move || {
        let Some(strong) = notch.upgrade() else {
            return;
        };
        match NotchWindow::attach(strong.window()) {
            Ok(native) => ready(strong, native),
            Err(err) if attempts_left == 0 => {
                panic!("slint never created a Win32 window: {err:#}")
            }
            Err(_) => on_native_window(notch, attempts_left - 1, ready),
        }
    });
}
