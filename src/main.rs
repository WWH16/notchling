#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod gpu;
mod hook;
mod pipe;
mod process;
mod protocol;
mod requests;
mod server;
mod system;
mod window;

#[allow(missing_debug_implementations, reason = "slint-generated types")]
mod ui {
    slint::include_modules!();
}

use std::env;
use std::mem;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpu::GpuLoad;
use pipe::Listener;
use protocol::{Decision, Event, Origin};
use requests::{RequestId, Requests};
use server::Update;
use slint::{ComponentHandle, Timer, TimerMode, Weak};
use system::SystemLoad;
use ui::{AlertKind, Choice, Notch};
use window::{MenuChoice, NotchWindow};

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

fn main() -> anyhow::Result<()> {
    if env::args().nth(1).as_deref() == Some("--hook") {
        hook::run();
        return Ok(());
    }

    // Claiming the pipe first also stops a second notch from starting.
    let listener = Listener::bind(&pipe::pipe_name()?)?;

    let notch = Notch::new()?;
    window::place_top_center(notch.window(), notch.get_fixed_width())?;
    notch.show()?;

    on_native_window(notch.as_weak(), NATIVE_WINDOW_ATTEMPTS, |notch, native| {
        native.apply_tool_styles();
        let native = Rc::new(native);

        let menu_native = Rc::clone(&native);
        notch.on_menu_requested(move || {
            if menu_native.show_menu() == Some(MenuChoice::Quit) {
                // Only fails when the loop already stopped, which is what we want anyway.
                let _ = slint::quit_event_loop();
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
    });

    let _stats = start_stats(&notch);
    serve_hooks(&notch, listener);

    slint::run_event_loop()?;
    Ok(())
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

/// Accepts hook connections and shows their permission requests on the notch.
fn serve_hooks(notch: &Notch, listener: Listener) {
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

    let weak = notch.as_weak();
    let shown = Arc::clone(&requests);
    server::spawn(listener, requests, move |update| {
        let requests = Arc::clone(&shown);
        let alert = Arc::clone(&alert);
        // Fails only once the event loop has quit, when nothing needs drawing.
        let _ = weak.upgrade_in_event_loop(move |notch| match update {
            Update::Requests => {
                show_requests(&notch, &requests.lock().expect("requests lock poisoned"));
            }
            Update::Session(Event::TaskDone { origin }) => {
                show_alert(&notch, &alert, AlertKind::Done, origin, DONE_ALERT);
            }
            Update::Session(Event::NeedsInput { origin, .. }) => {
                show_alert(
                    &notch,
                    &alert,
                    AlertKind::NeedsInput,
                    origin,
                    NEEDS_INPUT_ALERT,
                );
            }
            // Keep-awake arrives in step 2.6.
            Update::Session(Event::PromptSubmitted { .. }) => {}
            // The server turns these into `Update::Requests`.
            Update::Session(Event::PermissionRequest { .. }) => {}
        });
    });
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
