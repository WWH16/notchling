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
mod settings;
mod status;
mod system;
mod toast;
mod usage;
mod window;

#[allow(missing_debug_implementations, reason = "slint-generated types")]
mod ui {
    slint::include_modules!();
}

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::env;
use std::iter;
use std::mem;
use std::rc::Rc;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use awake::KeepAwake;
use chrono::{DateTime, Local, Utc};
use config::Config;
use gpu::GpuLoad;
use pipe::Listener;
use protocol::{Decision, Event, Question};
use requests::{Answering, Ask, RequestId, Requests};
use server::Update;
use slint::{ComponentHandle, ModelRc, Timer, TimerMode, VecModel, Weak};
use status::Health;
use system::SystemLoad;
use ui::{AlertKind, Choice, Notch, RequestKind};
use usage::{Claude, Codex, Crossing, Gemini, Provider, Reading, Thresholds, Usage};
use window::{Edge, MenuChoice, MessageKind, NotchWindow, Placement, Shortcut};
use windows::Win32::Foundation::POINT;

/// One frame at 60 Hz; how often to check whether the native window exists yet.
const FRAME: Duration = Duration::from_millis(16);

/// About one second of frames before giving up on the native window.
const NATIVE_WINDOW_ATTEMPTS: u32 = 60;

/// How often the CPU, GPU and RAM readings refresh, matching Task Manager's default.
const STATS_INTERVAL: Duration = Duration::from_secs(1);

/// How long a shrinking shape keeps its old window region: the longest morph animation.
/// Updating the region on every animation frame made the notch flicker.
const SHRINK_DELAY: Duration = Duration::from_millis(350);

/// How long after a move to another display the notch re-clips its window, once
/// Windows has applied that display's scale.
const MONITOR_MOVE_SETTLE: Duration = Duration::from_millis(200);

/// How long the notch takes to glide onto an edge once let go; as long as its morph.
const GLIDE: Duration = Duration::from_millis(260);

/// How often the notch checks for a fullscreen app to hide from.
const FULLSCREEN_CHECK: Duration = Duration::from_secs(1);

/// How often the notch re-reads Windows settings it follows, such as "Show animations".
const SYSTEM_SETTINGS_CHECK: Duration = Duration::from_secs(2);

/// How long "task done" stays on the notch.
const DONE_ALERT: Duration = Duration::from_secs(5);

/// How long "needs your input" stays; longer, since it waits on the user.
const NEEDS_INPUT_ALERT: Duration = Duration::from_secs(15);

/// How long a usage alert stays on the notch.
const USAGE_ALERT: Duration = Duration::from_secs(10);

/// How often silent sessions are checked against the keep-awake safety window, and the
/// working time on the notch ticks; it shows whole minutes.
const AWAKE_CHECK: Duration = Duration::from_secs(15);

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
    let shown = config.shown_provider();
    let usages = Arc::new(Mutex::new(Usages::new(
        shown,
        config.providers(),
        Thresholds::new(config.usage_alerts()),
    )));
    // Known before any source runs, so an alert at startup during a game becomes a toast.
    let alert = Arc::new(Mutex::new(Alert {
        hidden: window::fullscreen_app_in_front(),
        ..Alert::default()
    }));

    let notch = Notch::new()?;
    let placement = Rc::new(RefCell::new(Placement {
        device: config.monitor().map(str::to_owned),
        edge: config.edge(),
        position: config.position(),
    }));
    placement
        .borrow_mut()
        .apply(notch.window(), window_size(&notch))?;
    show_placement(&notch, &placement.borrow());
    show_preferences(&notch, &config);
    if let Some(provider) = shown {
        show_usage(&notch, provider, None);
    }
    notch.show()?;
    let status_switch = start_status(&notch, &usages);

    let menu_usages = Arc::clone(&usages);
    let fullscreen_check = Rc::new(Timer::default());
    let fullscreen_timer = Rc::clone(&fullscreen_check);
    let fullscreen_alert = Arc::clone(&alert);
    on_native_window(
        notch.as_weak(),
        NATIVE_WINDOW_ATTEMPTS,
        move |notch, native| {
            native.apply_tool_styles();
            let native = Rc::new(native);
            hide_during_fullscreen(&fullscreen_timer, Rc::clone(&native), fullscreen_alert);

            let clip = clip_to_shape(&notch, Rc::clone(&native));
            clip();
            let shape_clip = Rc::clone(&clip);
            notch.on_shape_changed(move || shape_clip());
            drag_and_stick(&notch, &placement, &clip);
            answer_with_shortcuts(&notch, &native);
            let typing = Rc::clone(&native);
            notch.on_reply_focus_requested(move || typing.activate());
            let weak = notch.as_weak();
            let resized = Rc::clone(&placement);
            let settled = Rc::clone(&clip);
            notch.on_size_changed(move || {
                if let Some(notch) = weak.upgrade() {
                    replace(&notch, &mut resized.borrow_mut(), &settled);
                }
            });

            let weak = notch.as_weak();
            let settings = RefCell::new(None);
            notch.on_menu_requested(move || {
                let Some(notch) = weak.upgrade() else {
                    return;
                };
                // Copied out: the menu waits for a pick, and sources check `tracked` meanwhile.
                let (shown, tracked) = {
                    let usages = lock_usages(&menu_usages);
                    (usages.shown, usages.tracked.clone())
                };
                match window::show_menu(shown, &tracked) {
                    Some(MenuChoice::Quit) => {
                        // Only fails when the loop already stopped, which is what we want anyway.
                        let _ = slint::quit_event_loop();
                    }
                    Some(MenuChoice::Show(provider)) => {
                        select_provider(&notch, &menu_usages, &status_switch, Some(provider));
                    }
                    Some(MenuChoice::Settings) => {
                        let saved =
                            apply_settings(&notch, &menu_usages, &status_switch, &placement, &clip);
                        // A window that cannot open leaves everything as it is.
                        let _ = settings::open(&settings, saved);
                    }
                    None => {}
                }
            });
        },
    );

    let _stats = start_stats(&notch);
    let _system_settings = follow_system_settings(&notch);
    start_usage(&notch, &usages, &alert);
    let _awake_check = serve_hooks(&notch, listener, &alert);

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
                "The notchling hooks were {verb}\n{}\n\n{backup}\n\nClaude Code picks up the change automatically.",
                outcome.settings.display()
            );
            window::message_box("notchling", &text, MessageKind::Info);
        }
        Err(err) => {
            let text = format!("Nothing was changed.\n\n{err:#}");
            window::message_box("notchling", &text, MessageKind::Error);
        }
    }
}

/// Hides the notch while a fullscreen app, game or presentation is in front, checking
/// until `timer` is dropped. Meanwhile alerts become toasts.
fn hide_during_fullscreen(timer: &Timer, native: Rc<NotchWindow>, alert: Arc<Mutex<Alert>>) {
    let hidden = Cell::new(false);
    let check = move || {
        let hide = window::fullscreen_app_in_front();
        if hide != hidden.replace(hide) {
            alert.lock().expect("alert lock poisoned").hidden = hide;
            if hide {
                native.hide();
            } else {
                native.show();
            }
        }
    };
    // Right away too, so a notch started during a game never shows.
    check();
    timer.start(TimerMode::Repeated, FULLSCREEN_CHECK, check);
}

/// Keeps the window's clickable region on the notch's shape, returning the function
/// that updates it. A growing shape gets its room at once, so the morph is never cut
/// off; a shrinking one keeps the old room until the morph has finished.
fn clip_to_shape(notch: &Notch, native: Rc<NotchWindow>) -> Rc<dyn Fn()> {
    let weak = notch.as_weak();
    // Both the size the shape heads to and the size it has now, against its edge: when
    // the notch sticks to another edge, the morph starts from the old size.
    let target = Rc::new(move || {
        let notch = weak.upgrade()?;
        let rect = |width, height| {
            window::shape_rect(
                window_size(&notch),
                slint::LogicalSize::new(width, height),
                notch.window().scale_factor(),
                Edge::from(notch.get_edge()),
            )
        };
        Some(window::union(
            rect(notch.get_shape_width(), notch.get_shape_height()),
            rect(notch.get_drawn_width(), notch.get_drawn_height()),
        ))
    });
    let applied = Rc::new(Cell::new(None));
    let shrink = Rc::new(Timer::default());
    Rc::new(move || {
        let Some(wanted) = target() else {
            return;
        };
        let room = applied
            .get()
            .map_or(wanted, |current| window::union(current, wanted));
        if applied.get() != Some(room) {
            native.clip_to(room);
            applied.set(Some(room));
        }
        if room == wanted {
            shrink.stop();
            return;
        }
        let (native, applied, target) =
            (Rc::clone(&native), Rc::clone(&applied), Rc::clone(&target));
        shrink.start(TimerMode::SingleShot, SHRINK_DELAY, move || {
            if let Some(wanted) = target() {
                native.clip_to(wanted);
                applied.set(Some(wanted));
            }
        });
    })
}

/// Lets Ctrl+Alt+Y and Ctrl+Alt+N answer the request on the notch, when the config allows
/// it. The keys are claimed only while a request shows.
fn answer_with_shortcuts(notch: &Notch, native: &Rc<NotchWindow>) {
    let weak = notch.as_weak();
    native.on_shortcut(move |shortcut| {
        let weak = weak.clone();
        // Answered once the window procedure has returned, not inside it.
        Timer::single_shot(Duration::ZERO, move || {
            if let Some(notch) = weak.upgrade() {
                notch.invoke_answer(shortcut.into());
            }
        });
    });

    let weak = notch.as_weak();
    let native = Rc::clone(native);
    notch.on_request_shown_changed(move || {
        let Some(notch) = weak.upgrade() else {
            return;
        };
        let enabled = notch.get_has_request() && Config::load().request_shortcuts();
        if enabled {
            native.claim_shortcuts();
        } else {
            native.release_shortcuts();
        }
        notch.set_shortcuts(enabled);
    });
}

/// The notch window's fixed size in logical pixels.
fn window_size(notch: &Notch) -> slint::LogicalSize {
    slint::LogicalSize::new(notch.get_fixed_width(), notch.get_fixed_height())
}

/// Tells the notch which edge it sticks to.
fn show_placement(notch: &Notch, placement: &Placement) {
    notch.set_edge(placement.edge.into());
}

/// Lets the user drag the notch anywhere, onto any display. While dragged it floats
/// with the cursor; let go, it glides onto the screen edge nearest the cursor, and that
/// spot is remembered.
fn drag_and_stick(notch: &Notch, placement: &Rc<RefCell<Placement>>, clip: &Rc<dyn Fn()>) {
    // Where the cursor and the window were when the button went down. The drag follows
    // the cursor on screen, so the pill resizing under it does not matter.
    let start: Rc<Cell<Option<(POINT, slint::PhysicalPosition)>>> = Rc::new(Cell::new(None));
    let glide = Rc::new(Timer::default());
    let started = Rc::clone(&start);
    let weak = notch.as_weak();
    notch.on_drag_started(move || {
        let window = weak.upgrade().map(|notch| notch.window().position());
        started.set(window::cursor().zip(window));
    });

    let weak = notch.as_weak();
    let dragging = Rc::clone(&start);
    let caught = Rc::clone(&glide);
    notch.on_dragged(move || {
        let (Some(notch), Some((cursor_start, window_start)), Some(cursor)) =
            (weak.upgrade(), dragging.get(), window::cursor())
        else {
            return;
        };
        if !notch.get_floating() && !window::past_drag_threshold(cursor_start, cursor) {
            return;
        }
        // Picked up mid-glide, it stays in the hand; a mere click lets the glide finish.
        caught.stop();
        notch.set_floating(true);
        notch.window().set_position(slint::PhysicalPosition::new(
            window_start.x + cursor.x - cursor_start.x,
            window_start.y + cursor.y - cursor_start.y,
        ));
    });

    let weak = notch.as_weak();
    let placement = Rc::clone(placement);
    let clip = Rc::clone(clip);
    notch.on_drag_ended(move || {
        // A click without a drag leaves the notch where it is.
        let (Some(notch), Some(cursor)) = (weak.upgrade(), window::cursor()) else {
            return;
        };
        if !notch.get_floating() {
            return;
        }
        notch.set_floating(false);
        let window = notch.window();
        let size = window_size(&notch);
        let mut placement = placement.borrow_mut();
        let from_edge = placement.edge;
        // Fails only when Windows reports no display; the notch then stays where it was let go.
        let Ok(target) = placement.stick_near(size, cursor) else {
            return;
        };
        // The pill moves within the window when the edge changes, so shift the window to
        // keep the pill where it was let go; from there it glides and morphs onto the edge.
        let drawn = slint::LogicalSize::new(notch.get_drawn_width(), notch.get_drawn_height());
        let scale = window.scale_factor();
        let was = window::center(window::shape_rect(size, drawn, scale, from_edge));
        let will_be = window::center(window::shape_rect(size, drawn, scale, placement.edge));
        let position = window.position();
        let from = slint::PhysicalPosition::new(
            position.x + was.x - will_be.x,
            position.y + was.y - will_be.y,
        );
        window.set_position(from);
        show_placement(&notch, &placement);
        clip();
        glide_window(&notch, &glide, from, target);
        // A move to a display with another scale resizes the window a moment later.
        let settled = Rc::clone(&clip);
        Timer::single_shot(GLIDE + MONITOR_MOVE_SETTLE, move || settled());
        let spot = placement.clone();
        // On failure the spot only lasts until the notch restarts.
        let _ = Config::update(|config| {
            config.set_monitor(spot.device);
            config.set_edge(spot.edge);
            config.set_position(spot.position);
        });
    });
}

/// Moves the notch window from `from` to `to` over `GLIDE`, easing out, unless `timer`
/// is stopped or restarted first. With Windows' animations off it jumps there.
fn glide_window(
    notch: &Notch,
    timer: &Rc<Timer>,
    from: slint::PhysicalPosition,
    to: slint::PhysicalPosition,
) {
    if notch.global::<ui::Theme>().get_reduce_motion() {
        timer.stop();
        notch.window().set_position(to);
        return;
    }
    let weak = notch.as_weak();
    let own = Rc::downgrade(timer);
    let started = Instant::now();
    timer.start(TimerMode::Repeated, FRAME, move || {
        let progress = (started.elapsed().as_secs_f32() / GLIDE.as_secs_f32()).min(1.0);
        // Cubic ease-out: quick off the hand, settling softly onto the edge.
        let eased = 1.0 - (1.0 - progress).powi(3);
        // Screen pixels are far inside f32's exact range; rounding to whole ones is intended.
        let step = |from: i32, to: i32| from + ((to - from) as f32 * eased).round() as i32;
        if let Some(notch) = weak.upgrade() {
            let position = slint::PhysicalPosition::new(step(from.x, to.x), step(from.y, to.y));
            notch.window().set_position(position);
        }
        if progress >= 1.0
            && let Some(timer) = own.upgrade()
        {
            timer.stop();
        }
    });
}

/// Applies saved settings to the running notch: its display, tucking, the readings it
/// shows, the tracked providers and the usage alerts. The usage sources and hook mode
/// read the rest from the config file themselves.
fn apply_settings(
    notch: &Notch,
    usages: &Arc<Mutex<Usages>>,
    status_switch: &Sender<Option<Provider>>,
    placement: &Rc<RefCell<Placement>>,
    clip: &Rc<dyn Fn()>,
) -> impl Fn(&Config) + 'static {
    let weak = notch.as_weak();
    let usages = Arc::clone(usages);
    let status_switch = status_switch.clone();
    let placement = Rc::clone(placement);
    let clip = Rc::clone(clip);
    move |config| {
        let shown = {
            let mut usages = lock_usages(&usages);
            usages.thresholds = Thresholds::new(config.usage_alerts());
            usages.tracked = config.providers();
            // An untracked provider's last numbers would go stale, so they go.
            usages.latest.retain(|&provider, _| config.tracks(provider));
            usages.shown
        };
        let Some(notch) = weak.upgrade() else {
            return;
        };
        show_preferences(&notch, config);
        let next = config.shown_provider();
        if next != shown {
            select_provider(&notch, &usages, &status_switch, next);
        }
        let mut placement = placement.borrow_mut();
        placement.device = config.monitor().map(str::to_owned);
        replace(&notch, &mut placement, &clip);
    }
}

/// Moves the notch to its placement again, after its display or size changed, and
/// re-clips it once Windows has settled the move.
fn replace(notch: &Notch, placement: &mut Placement, clip: &Rc<dyn Fn()>) {
    // Fails only when Windows reports no display; the notch then stays put.
    let _ = placement.apply(notch.window(), window_size(notch));
    show_placement(notch, placement);
    let clip = Rc::clone(clip);
    Timer::single_shot(MONITOR_MOVE_SETTLE, move || clip());
}

/// Keeps the notch in line with Windows' "Show animations" and text size settings until
/// the returned timer is dropped.
fn follow_system_settings(notch: &Notch) -> Timer {
    notch
        .global::<ui::Theme>()
        .set_mono(window::mono_font().into());
    let apply = |notch: &Notch| {
        let theme = notch.global::<ui::Theme>();
        theme.set_reduce_motion(!window::animations_enabled());
        theme.set_text_scale(window::text_scale());
    };
    apply(notch);
    let timer = Timer::default();
    let weak = notch.as_weak();
    timer.start(TimerMode::Repeated, SYSTEM_SETTINGS_CHECK, move || {
        if let Some(notch) = weak.upgrade() {
            apply(&notch);
        }
    });
    timer
}

/// Applies the settings the notch reads directly: tucking and which readings show.
fn show_preferences(notch: &Notch, config: &Config) {
    notch.set_tuck(config.tuck());
    notch.set_show_usage(config.shown_provider().is_some());
    notch.set_show_cpu(config.show_cpu());
    notch.set_show_gpu(config.show_gpu());
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

        // GPU counters are the costlier read, so they rest while the GPU is hidden.
        if let Some(gpu) = gpu.as_mut().filter(|_| notch.get_show_gpu()) {
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

/// The latest good usage reading of each provider, which one the notch shows, which
/// ones are tracked, and which usage alerts already fired.
#[derive(Debug)]
struct Usages {
    /// `None` when no provider is tracked.
    shown: Option<Provider>,
    tracked: Vec<Provider>,
    latest: HashMap<Provider, Usage>,
    thresholds: Thresholds,
}

impl Usages {
    fn new(shown: Option<Provider>, tracked: Vec<Provider>, thresholds: Thresholds) -> Self {
        Self {
            shown,
            tracked,
            latest: HashMap::new(),
            thresholds,
        }
    }
}

fn lock_usages(usages: &Mutex<Usages>) -> std::sync::MutexGuard<'_, Usages> {
    usages.lock().expect("usages lock poisoned")
}

/// Polls every tracked provider's usage in the background and keeps the notch showing
/// the selected one. A provider whose source cannot start shows as "no data".
fn start_usage(notch: &Notch, usages: &Arc<Mutex<Usages>>, alert: &Arc<Mutex<Alert>>) {
    let weak = notch.as_weak();
    let tracked = {
        let usages = Arc::clone(usages);
        move |provider| lock_usages(&usages).tracked.contains(&provider)
    };
    let usages = Arc::clone(usages);
    let alert = Arc::clone(alert);
    let notify = move |reading: Reading| {
        let usages = Arc::clone(&usages);
        let alert = Arc::clone(&alert);
        // Fails only once the event loop has quit, when nothing needs drawing.
        let _ = weak.upgrade_in_event_loop(move |notch| {
            let mut usages = lock_usages(&usages);
            // A read that was under way when its provider was unchecked.
            if !usages.tracked.contains(&reading.provider) {
                return;
            }
            // A failed read drops the old numbers instead of leaving them up as current.
            match reading.usage {
                Ok(usage) => {
                    if let Some(crossing) = usages.thresholds.crossed(reading.provider, &usage) {
                        let text = usage_alert_text(reading.provider, &usage);
                        let kind = alert_kind(crossing);
                        show_alert(&notch, &alert, kind, reading.provider, text, Vec::new());
                    }
                    usages.latest.insert(reading.provider, usage);
                }
                Err(_) => {
                    usages.latest.remove(&reading.provider);
                }
            }
            if Some(reading.provider) == usages.shown {
                show_usage(
                    &notch,
                    reading.provider,
                    usages.latest.get(&reading.provider),
                );
            }
        });
    };
    if let Ok(source) = Claude::try_new() {
        usage::spawn(source, CLAUDE_INTERVAL, tracked.clone(), notify.clone());
    }
    if let Ok(source) = Codex::try_new() {
        usage::spawn(source, CLI_LOG_INTERVAL, tracked.clone(), notify.clone());
    }
    if let Ok(source) = Gemini::try_new() {
        usage::spawn(source, CLI_LOG_INTERVAL, tracked, notify);
    }
}

/// Checks the shown provider's status page in the background and shows incidents on
/// the notch. Returns where to send the provider whenever the selection changes.
fn start_status(notch: &Notch, usages: &Arc<Mutex<Usages>>) -> Sender<Option<Provider>> {
    let weak = notch.as_weak();
    let usages = Arc::clone(usages);
    let shown = lock_usages(&usages).shown;
    status::spawn(shown, move |provider, health| {
        let usages = Arc::clone(&usages);
        // Fails only once the event loop has quit, when nothing needs drawing.
        let _ = weak.upgrade_in_event_loop(move |notch| {
            // A status that arrives after a switch belongs to the old provider.
            if lock_usages(&usages).shown == Some(provider) {
                notch.set_status(health.map_or(ui::Health::Unknown, ui::Health::from));
            }
        });
    })
}

/// Shows `provider` on the notch from now on, and remembers the choice in the config.
/// `None`, when no provider is tracked, keeps the old choice for when one is again.
fn select_provider(
    notch: &Notch,
    usages: &Mutex<Usages>,
    status_switch: &Sender<Option<Provider>>,
    provider: Option<Provider>,
) {
    let mut usages = lock_usages(usages);
    usages.shown = provider;
    notch.set_status(ui::Health::Unknown);
    // Fails only if the status thread is gone, which leaves the status unknown.
    let _ = status_switch.send(provider);
    let Some(provider) = provider else {
        return;
    };
    show_usage(notch, provider, usages.latest.get(&provider));
    // On failure the choice only lasts until the notch restarts.
    let _ = Config::update(|config| config.set_provider(provider));
}

/// "Claude at 82%, resets 2:20 PM".
fn usage_alert_text(provider: Provider, usage: &Usage) -> String {
    let used = usage.used.unwrap_or(0.0) * 100.0;
    match reset_text(usage.resets_at) {
        Some(reset) => format!("{} at {used:.0}%, {reset}", provider.name()),
        None => format!("{} at {used:.0}%", provider.name()),
    }
}

/// "resets 2:20 PM", in the PC's time zone.
fn reset_text(at: Option<DateTime<Utc>>) -> Option<String> {
    let at = at?.with_timezone(&Local);
    Some(format!("resets {}", usage::reset_time(at, &Local::now())))
}

/// "85%", or "-" when the share is unknown.
fn percent_text(used: Option<f32>) -> String {
    used.map_or_else(|| "-".to_owned(), |used| format!("{:.0}%", used * 100.0))
}

/// The name of the one limit a source knows when it reports no list of them: Claude's
/// 5-hour session from the local logs, the Codex plan, and Gemini CLI's day.
fn window_label(provider: Provider) -> &'static str {
    match provider {
        Provider::Claude => "Current session",
        Provider::ChatGpt => "Plan limit",
        Provider::Gemini => "Today",
    }
}

/// The limits the hover panel lists: every one the provider reported, or the one the
/// pill follows when the source knows no others. Without a reading, one empty limit.
fn panel_limits(provider: Provider, usage: Option<&Usage>) -> Vec<usage::Limit> {
    match usage {
        Some(usage) if !usage.limits.is_empty() => usage.limits.clone(),
        _ => vec![usage::Limit {
            group: None,
            name: window_label(provider).to_owned(),
            used: usage.and_then(|usage| usage.used).unwrap_or(-1.0),
            resets_at: usage.and_then(|usage| usage.resets_at),
        }],
    }
}

/// The panel's lines: each model group's title on a line of its own, then its limits
/// `per_line` at a time.
fn limit_lines(
    limits: &[usage::Limit],
    per_line: usize,
    no_data: bool,
    now: &DateTime<Local>,
) -> Vec<ui::LimitLine> {
    let view = |limit: &usage::Limit| {
        let (used_text, left_text) = limit_texts(limit.used, no_data);
        ui::LimitView {
            name: limit.name.as_str().into(),
            used: limit.used,
            reset: limit
                .resets_at
                .map(|at| usage::reset_label(at.with_timezone(&Local), now))
                .unwrap_or_default()
                .into(),
            used_text: used_text.into(),
            left_text: left_text.into(),
        }
    };
    let mut lines = Vec::new();
    for group in limits.chunk_by(|a, b| a.group == b.group) {
        if let Some(title) = &group[0].group {
            lines.push(ui::LimitLine {
                title: title.as_str().into(),
                ..ui::LimitLine::default()
            });
        }
        for chunk in group.chunks(per_line) {
            lines.push(ui::LimitLine {
                left: view(&chunk[0]),
                right: chunk.get(1).map(view).unwrap_or_default(),
                paired: chunk.len() > 1,
                half: per_line > 1 && chunk.len() == 1,
                ..ui::LimitLine::default()
            });
        }
    }
    lines
}

/// "23% used" and "77% left", or nothing used and why the share is missing.
fn limit_texts(used: f32, no_data: bool) -> (String, String) {
    if no_data {
        return (String::new(), "No data".to_owned());
    }
    if used < 0.0 {
        return (String::new(), "Share unknown".to_owned());
    }
    // A share in 0..=1, so the percentage fits easily; rounding is intended.
    let used = (used * 100.0).round().clamp(0.0, 100.0) as u32;
    (format!("{used}% used"), format!("{}% left", 100 - used))
}

/// Amber for the lower thresholds, red for the last one.
fn alert_kind(crossing: Crossing) -> AlertKind {
    if crossing.last {
        AlertKind::UsageCritical
    } else {
        AlertKind::UsageHigh
    }
}

/// Puts a provider's usage on the notch, or "no data" when there is no good reading.
/// The pill shows one share; the hover panel lists every limit, with tokens and burn
/// rate when the source counts them.
fn show_usage(notch: &Notch, provider: Provider, usage: Option<&Usage>) {
    notch.set_usage_provider(provider.into());
    let limits = panel_limits(provider, usage);
    let now = Local::now();
    let lines = |per_line| {
        let lines = limit_lines(&limits, per_line, usage.is_none(), &now);
        ModelRc::new(VecModel::from(lines))
    };
    notch.set_wide_lines(lines(2));
    notch.set_tall_lines(lines(1));
    let Some(usage) = usage else {
        notch.set_usage_fraction(-1.0);
        notch.set_usage_short("-".into());
        notch.set_usage_tokens("".into());
        notch.set_usage_rate("".into());
        notch.set_usage_via("".into());
        notch.set_usage_estimated(false);
        return;
    };
    notch.set_usage_fraction(usage.used.unwrap_or(-1.0));
    notch.set_usage_short(percent_text(usage.used).into());
    let via = usage.via.map(|via| format!("via {via}"));
    notch.set_usage_via(via.unwrap_or_default().into());

    let tokens = usage
        .tokens
        .map(|tokens| format!("{} tokens", usage::compact(tokens)));
    // Float to integer casts saturate; display only.
    let rate = usage
        .burn_rate
        .map(|rate| format!("{}/min", usage::compact(rate.round() as u64)));
    notch.set_usage_tokens(tokens.unwrap_or_default().into());
    notch.set_usage_rate(rate.unwrap_or_default().into());
    notch.set_usage_estimated(usage.estimated);
}

/// Accepts hook connections and reacts to their events on the notch. Returns the
/// timer that releases keep-awake for silent sessions; it runs until dropped.
fn serve_hooks(notch: &Notch, listener: Listener, alert: &Arc<Mutex<Alert>>) -> Timer {
    let requests = Arc::new(Mutex::new(Requests::default()));

    let weak = notch.as_weak();
    let decided = Arc::clone(&requests);
    notch.on_decide(move |id, choice| {
        let Some(notch) = weak.upgrade() else {
            return;
        };
        let mut requests = decided.lock().expect("requests lock poisoned");
        requests.decide(RequestId::from_ui(id), Decision::from(choice));
        show_requests(&notch, &mut requests);
    });

    let weak = notch.as_weak();
    let picked = Arc::clone(&requests);
    notch.on_option_picked(move |id, option| {
        if let (Some(notch), Ok(option)) = (weak.upgrade(), usize::try_from(option)) {
            answer_question(&notch, &picked, RequestId::from_ui(id), Some(option));
        }
    });
    let weak = notch.as_weak();
    let confirmed = Arc::clone(&requests);
    notch.on_question_confirmed(move |id| {
        if let Some(notch) = weak.upgrade() {
            answer_question(&notch, &confirmed, RequestId::from_ui(id), None);
        }
    });

    let weak = notch.as_weak();
    let replied = Arc::clone(&requests);
    notch.on_reply_sent(move |id, text| {
        let message = text.trim();
        let Some(notch) = weak.upgrade().filter(|_| !message.is_empty()) else {
            return;
        };
        let mut requests = replied.lock().expect("requests lock poisoned");
        let decision = Decision::Replied {
            message: message.to_owned(),
        };
        requests.decide(RequestId::from_ui(id), decision);
        show_requests(&notch, &mut requests);
    });
    let weak = notch.as_weak();
    let opened = Arc::clone(&requests);
    notch.on_reply_opened(move |id| {
        let Some(notch) = weak.upgrade() else {
            return;
        };
        let mut requests = opened.lock().expect("requests lock poisoned");
        let ancestors = match requests.front() {
            Some(front) if front.id == RequestId::from_ui(id) => match &front.request.ask {
                Ask::Reply { ancestors, .. } => ancestors.clone(),
                _ => Vec::new(),
            },
            _ => Vec::new(),
        };
        requests.decide(RequestId::from_ui(id), Decision::AskInApp);
        show_requests(&notch, &mut requests);
        drop(requests);
        process::focus_first_window(&ancestors);
    });

    let clicked = Arc::clone(alert);
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
    let weak = notch.as_weak();
    awake_check.start(TimerMode::Repeated, AWAKE_CHECK, move || {
        let mut awake = lock_awake(&expiring);
        awake.expire(Instant::now());
        if let Some(notch) = weak.upgrade() {
            show_activity(&notch, &awake);
        }
    });

    let weak = notch.as_weak();
    let shown = Arc::clone(&requests);
    let alert = Arc::clone(alert);
    server::spawn(listener, requests, move |update| {
        let requests = Arc::clone(&shown);
        let alert = Arc::clone(&alert);
        let awake = Arc::clone(&awake);
        // Fails only once the event loop has quit, when nothing needs drawing.
        let _ = weak.upgrade_in_event_loop(move |notch| match update {
            Update::Requests => {
                show_requests(
                    &notch,
                    &mut requests.lock().expect("requests lock poisoned"),
                );
            }
            Update::Session(Event::TaskDone { origin, .. }) => {
                let mut awake = lock_awake(&awake);
                awake.stopped(&origin.session_id);
                show_activity(&notch, &awake);
                // The reply card says it is done; behind a fullscreen app nobody can
                // reply, so Claude stops at once and a toast says so instead.
                let hidden = alert.lock().expect("alert lock poisoned").hidden;
                if hidden {
                    let mut requests = requests.lock().expect("requests lock poisoned");
                    requests.settle_session(&origin.session_id);
                    show_requests(&notch, &mut requests);
                    let text = session_alert_text(AlertKind::Done, &origin.project);
                    show_alert(
                        &notch,
                        &alert,
                        AlertKind::Done,
                        Provider::Claude,
                        text,
                        origin.ancestors,
                    );
                }
            }
            Update::Session(Event::NeedsInput { origin, .. }) => {
                let mut awake = lock_awake(&awake);
                awake.stopped(&origin.session_id);
                show_activity(&notch, &awake);
                let text = session_alert_text(AlertKind::NeedsInput, &origin.project);
                show_alert(
                    &notch,
                    &alert,
                    AlertKind::NeedsInput,
                    Provider::Claude,
                    text,
                    origin.ancestors,
                );
            }
            Update::Session(Event::PromptSubmitted { origin }) => {
                let mut awake = lock_awake(&awake);
                awake.started(origin.session_id, Instant::now());
                show_activity(&notch, &awake);
            }
            // The server turns these into `Update::Requests`.
            Update::Session(
                Event::PermissionRequest { .. }
                | Event::Question { .. }
                | Event::ToolSettled { .. },
            ) => {}
        });
    });
    awake_check
}

fn lock_awake(awake: &Mutex<KeepAwake>) -> std::sync::MutexGuard<'_, KeepAwake> {
    awake.lock().expect("keep-awake lock poisoned")
}

/// Shows how long Claude has been working on the idle notch, or nothing while it rests.
fn show_activity(notch: &Notch, awake: &KeepAwake) {
    let (sessions, time) = awake
        .activity(Instant::now())
        .map_or((0, String::new()), |(sessions, longest)| {
            (sessions, working_time(longest))
        });
    notch.set_activity(time.into());
    notch.set_activity_sessions(i32::try_from(sessions).unwrap_or(i32::MAX));
}

/// "<1m", "3m" or "1h 05m": how long a session has worked, in whole minutes.
fn working_time(elapsed: Duration) -> String {
    let minutes = elapsed.as_secs() / 60;
    match minutes {
        0 => "<1m".to_owned(),
        1..60 => format!("{minutes}m"),
        _ => format!("{}h {:02}m", minutes / 60, minutes % 60),
    }
}

/// The alert on screen: which one it is, and the session a click focuses, if any.
#[derive(Debug, Default)]
struct Alert {
    /// Bumped per alert, so an old alert's timer cannot hide a newer one.
    serial: u64,
    ancestors: Vec<u32>,
    /// The notch hides behind a fullscreen app, so alerts go to toasts instead.
    hidden: bool,
}

/// "Done in notchling" or "notchling needs your input", or without the project when it is
/// unknown. Usage alerts have their own text.
fn session_alert_text(kind: AlertKind, project: &str) -> String {
    match (kind, project.is_empty()) {
        (AlertKind::NeedsInput, true) => "Needs your input".to_owned(),
        (AlertKind::NeedsInput, false) => format!("{project} needs your input"),
        (_, true) => "Done".to_owned(),
        (_, false) => format!("Done in {project}"),
    }
}

/// Shows an attention message about `provider` until it times out or is clicked. A click
/// focuses the first window among `ancestors`, the processes above a Claude Code hook.
/// While the notch hides behind a fullscreen app, the message becomes a Windows toast.
fn show_alert(
    notch: &Notch,
    alert: &Arc<Mutex<Alert>>,
    kind: AlertKind,
    provider: Provider,
    text: String,
    ancestors: Vec<u32>,
) {
    let serial = {
        let mut current = alert.lock().expect("alert lock poisoned");
        if current.hidden {
            // A toast that fails to show is lost; the notch has nowhere to put it either.
            let _ = toast::show(&text);
            return;
        }
        current.serial += 1;
        current.ancestors = ancestors;
        current.serial
    };
    let duration = match kind {
        AlertKind::Done => DONE_ALERT,
        AlertKind::NeedsInput => NEEDS_INPUT_ALERT,
        AlertKind::UsageHigh | AlertKind::UsageCritical => USAGE_ALERT,
    };
    notch.set_alert_kind(kind);
    notch.set_alert_provider(provider.into());
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
/// A request with questions shows the one its answering has reached.
fn show_requests(notch: &Notch, requests: &mut Requests) {
    let progress = requests.answering.take();
    let Some(front) = requests.front() else {
        // The texts stay so the card can fade out with its content.
        notch.set_has_request(false);
        return;
    };
    notch.set_request_id(front.id.to_ui());
    notch.set_request_project(front.request.project.as_str().into());
    notch.set_request_more(i32::try_from(requests.behind_front()).unwrap_or(i32::MAX));
    let progress = match &front.request.ask {
        Ask::Permission {
            tool,
            description,
            detail,
        } => {
            notch.set_request_kind(RequestKind::Permission);
            notch.set_request_tool(tool.as_str().into());
            notch.set_request_description(description.as_str().into());
            notch.set_request_detail(detail.as_str().into());
            None
        }
        Ask::Questions(questions) => {
            let progress = progress
                .filter(|progress| progress.id == front.id)
                .unwrap_or_else(|| Answering::new(front.id, questions));
            show_question(notch, questions, &progress);
            notch.set_request_kind(RequestKind::Question);
            Some(progress)
        }
        Ask::Reply { message, .. } => {
            notch.set_reply_message(message.as_str().into());
            notch.set_request_kind(RequestKind::Reply);
            None
        }
    };
    notch.set_has_request(true);
    requests.answering = progress;
}

/// Puts the question `progress` has reached on the question card.
fn show_question(notch: &Notch, questions: &[Question], progress: &Answering) {
    let Some(question) = questions.get(progress.index) else {
        return;
    };
    let options: Vec<ui::OptionView> = question
        .options
        .iter()
        .zip(progress.picked.iter().chain(iter::repeat(&false)))
        .map(|(option, picked)| ui::OptionView {
            label: option.label.as_str().into(),
            description: option.description.as_str().into(),
            picked: *picked,
        })
        .collect();
    notch.set_question_text(question.question.as_str().into());
    notch.set_question_header(question.header.as_str().into());
    let step = if questions.len() > 1 {
        format!("{} of {}", progress.index + 1, questions.len())
    } else {
        String::new()
    };
    notch.set_question_step(step.into());
    notch.set_question_multi(question.multi_select);
    notch.set_question_options(ModelRc::new(VecModel::from(options)));
}

/// Picks option `option` of the question on request `id`'s card, or with no option,
/// confirms the ticked options of a multiple-choice question. Sends the answers once
/// every question has one.
fn answer_question(
    notch: &Notch,
    requests: &Mutex<Requests>,
    id: RequestId,
    option: Option<usize>,
) {
    let mut requests = requests.lock().expect("requests lock poisoned");
    let questions = match requests.front() {
        Some(front) if front.id == id => match &front.request.ask {
            // Cloned so the progress can change while the questions are read.
            Ask::Questions(questions) => questions.clone(),
            Ask::Permission { .. } | Ask::Reply { .. } => return,
        },
        _ => return,
    };
    let Some(progress) = requests
        .answering
        .as_mut()
        .filter(|progress| progress.id == id)
    else {
        return;
    };
    let finished = match option {
        Some(option) => progress.pick(&questions, option),
        None => progress.confirm(&questions),
    };
    if let Some(answers) = finished {
        requests.decide(id, answers);
    }
    show_requests(notch, &mut requests);
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

impl From<Edge> for ui::NotchEdge {
    fn from(edge: Edge) -> Self {
        match edge {
            Edge::Top => Self::Top,
            Edge::Bottom => Self::Bottom,
            Edge::Left => Self::Left,
            Edge::Right => Self::Right,
        }
    }
}

impl From<ui::NotchEdge> for Edge {
    fn from(edge: ui::NotchEdge) -> Self {
        match edge {
            ui::NotchEdge::Top => Self::Top,
            ui::NotchEdge::Bottom => Self::Bottom,
            ui::NotchEdge::Left => Self::Left,
            ui::NotchEdge::Right => Self::Right,
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

impl From<Shortcut> for Choice {
    fn from(shortcut: Shortcut) -> Self {
        match shortcut {
            Shortcut::Allow => Self::Allow,
            Shortcut::Deny => Self::Deny,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn working_time_shows_whole_minutes() {
        let cases = [
            (59, "<1m"),
            (180, "3m"),
            (3599, "59m"),
            (3900, "1h 05m"),
            (7200, "2h 00m"),
        ];
        for (secs, want) in cases {
            assert_eq!(working_time(Duration::from_secs(secs)), want, "{secs} s");
        }
    }

    fn limit(group: Option<&str>, name: &str, used: f32) -> usage::Limit {
        usage::Limit {
            group: group.map(str::to_owned),
            name: name.to_owned(),
            used,
            resets_at: None,
        }
    }

    /// Each line as text: a group's title, or its limits' names joined by "|", with "_"
    /// for the empty half of a wide line.
    fn layout(lines: &[ui::LimitLine]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                if !line.title.is_empty() {
                    return format!("# {}", line.title);
                }
                let mut names = vec![line.left.name.to_string()];
                if line.paired {
                    names.push(line.right.name.to_string());
                }
                if line.half {
                    names.push("_".to_owned());
                }
                names.join("|")
            })
            .collect()
    }

    #[test]
    fn grouped_limits_get_titles_and_pair_up_when_wide() {
        let limits = [
            limit(Some("Gemini Models"), "5-hour limit", 0.6),
            limit(Some("Gemini Models"), "Weekly limit", 0.2),
            limit(Some("Gemini 4 Argon"), "Weekly limit", 0.0),
        ];
        let now = Local::now();
        assert_eq!(
            layout(&limit_lines(&limits, 2, false, &now)),
            [
                "# Gemini Models",
                "5-hour limit|Weekly limit",
                "# Gemini 4 Argon",
                "Weekly limit|_"
            ]
        );
        assert_eq!(
            layout(&limit_lines(&limits, 1, false, &now)),
            [
                "# Gemini Models",
                "5-hour limit",
                "Weekly limit",
                "# Gemini 4 Argon",
                "Weekly limit"
            ]
        );
    }

    #[test]
    fn ungrouped_limits_have_no_title() {
        let limits = [
            limit(None, "Current session", 1.0),
            limit(None, "Weekly (all models)", 0.23),
        ];
        let lines = limit_lines(&limits, 2, false, &Local::now());
        assert_eq!(layout(&lines), ["Current session|Weekly (all models)"]);
        assert_eq!(
            (
                lines[0].left.used_text.as_str(),
                lines[0].left.left_text.as_str()
            ),
            ("100% used", "0% left")
        );
        assert_eq!(
            (
                lines[0].right.used_text.as_str(),
                lines[0].right.left_text.as_str()
            ),
            ("23% used", "77% left")
        );
    }

    #[test]
    fn a_source_without_a_list_shows_its_one_limit() {
        let estimate = Usage {
            used: None,
            ..Usage::default()
        };
        let limits = panel_limits(Provider::Claude, Some(&estimate));
        assert_eq!(limits, [limit(None, "Current session", -1.0)]);
        assert_eq!(limit_texts(-1.0, false).1, "Share unknown");
        assert_eq!(
            limit_texts(-1.0, true),
            (String::new(), "No data".to_owned())
        );
        assert_eq!(panel_limits(Provider::Gemini, None)[0].name, "Today");
    }

    #[test]
    fn session_alerts_read_as_sentences() {
        let cases = [
            (AlertKind::Done, "notchling", "Done in notchling"),
            (AlertKind::Done, "", "Done"),
            (
                AlertKind::NeedsInput,
                "notchling",
                "notchling needs your input",
            ),
            (AlertKind::NeedsInput, "", "Needs your input"),
        ];
        for (kind, project, want) in cases {
            assert_eq!(session_alert_text(kind, project), want, "{project:?}");
        }
    }
}
