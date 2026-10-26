#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod system;
mod window;

#[allow(missing_debug_implementations, reason = "slint-generated types")]
mod ui {
    slint::include_modules!();
}

use std::time::Duration;

use slint::{ComponentHandle, Timer, TimerMode, Weak};
use system::SystemLoad;
use ui::Notch;
use window::NotchWindow;

/// One frame at 60 Hz; how often to check whether the native window exists yet.
const FRAME: Duration = Duration::from_millis(16);

/// About one second of frames before giving up on the native window.
const NATIVE_WINDOW_ATTEMPTS: u32 = 60;

/// How often the CPU, GPU and RAM readings refresh, matching Task Manager's default.
const STATS_INTERVAL: Duration = Duration::from_secs(1);

fn main() -> anyhow::Result<()> {
    let notch = Notch::new()?;
    window::place_top_center(notch.window(), notch.get_fixed_width())?;
    notch.show()?;

    on_native_window(notch.as_weak(), NATIVE_WINDOW_ATTEMPTS, |notch, native| {
        native.apply_tool_styles();
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

    let stats_timer = Timer::default();
    let mut load = SystemLoad::new();
    let weak = notch.as_weak();
    stats_timer.start(TimerMode::Repeated, STATS_INTERVAL, move || {
        let Some(notch) = weak.upgrade() else {
            return;
        };
        let reading = load.sample();
        notch.set_cpu(reading.cpu);
        notch.set_ram(reading.ram_fraction());
        notch.set_ram_text(system::gigabytes(reading.ram_used_bytes).into());
    });

    slint::run_event_loop()?;
    Ok(())
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
