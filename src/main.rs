#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod gpu;
mod hook;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the notch side is wired up in step 2.4")
)]
mod pipe;
mod process;
mod protocol;
mod system;
mod window;

#[allow(missing_debug_implementations, reason = "slint-generated types")]
mod ui {
    slint::include_modules!();
}

use std::env;
use std::rc::Rc;
use std::time::Duration;

use gpu::GpuLoad;
use slint::{ComponentHandle, Timer, TimerMode, Weak};
use system::SystemLoad;
use ui::Notch;
use window::{MenuChoice, NotchWindow};

/// One frame at 60 Hz; how often to check whether the native window exists yet.
const FRAME: Duration = Duration::from_millis(16);

/// About one second of frames before giving up on the native window.
const NATIVE_WINDOW_ATTEMPTS: u32 = 60;

/// How often the CPU, GPU and RAM readings refresh, matching Task Manager's default.
const STATS_INTERVAL: Duration = Duration::from_secs(1);

fn main() -> anyhow::Result<()> {
    if env::args().nth(1).as_deref() == Some("--hook") {
        hook::run();
        return Ok(());
    }

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

    let stats_timer = Timer::default();
    let mut load = SystemLoad::new();
    // Without GPU counters the GPU and VRAM stats stay at their placeholders.
    let mut gpu = GpuLoad::open().ok();
    let weak = notch.as_weak();
    stats_timer.start(TimerMode::Repeated, STATS_INTERVAL, move || {
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
