//! Win32 window tweaks that Slint does not expose: tool-window styles and placement.

use std::ffi::c_void;
use std::mem;

use anyhow::{Context, bail};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    CreateRectRgn, DeleteObject, EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
    MONITORINFOEXW, SetWindowRgn,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Shell::{
    QUNS_BUSY, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN, SHQueryUserNotificationState,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CheckMenuRadioItem, CreatePopupMenu, DestroyMenu, GWL_EXSTYLE, GetCursorPos,
    GetWindowLongPtrW, HMENU, MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MF_BYCOMMAND, MF_SEPARATOR,
    MF_STRING, MONITORINFOF_PRIMARY, MessageBoxW, SW_HIDE, SW_SHOWNOACTIVATE, SetForegroundWindow,
    SetWindowLongPtrW, ShowWindow, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, WS_EX_APPWINDOW,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};
use windows::core::{BOOL, HSTRING, PCWSTR, w};

use crate::usage::Provider;

/// DPI that Windows treats as 100% scaling.
const BASE_DPI: f32 = 96.0;

/// Menu command id for [`MenuChoice::Quit`]; 0 is reserved for "dismissed".
const MENU_QUIT: usize = 1;

/// Menu command id for [`MenuChoice::Settings`].
const MENU_SETTINGS: usize = 2;

/// Menu command id of the first provider; the others follow in [`Provider::ALL`] order.
const MENU_FIRST_PROVIDER: usize = 10;

/// The icon of a [`message_box`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    Info,
    Error,
}

/// Shows a modal Windows message box and waits until it is closed.
pub fn message_box(title: &str, text: &str, kind: MessageKind) {
    let icon = match kind {
        MessageKind::Info => MB_ICONINFORMATION,
        MessageKind::Error => MB_ICONERROR,
    };
    // SAFETY: both strings are valid wide strings for the duration of the call.
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(text),
            &HSTRING::from(title),
            MB_OK | icon,
        )
    };
}

/// An item picked from the notch's right-click menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuChoice {
    /// Show this provider's usage on the notch.
    Show(Provider),
    Settings,
    Quit,
}

/// The cursor's horizontal screen position in physical pixels, if Windows reports it.
#[must_use]
pub fn cursor_x() -> Option<i32> {
    let mut cursor = POINT::default();
    // SAFETY: `cursor` is a live POINT the call writes into.
    unsafe { GetCursorPos(&mut cursor) }.ok()?;
    Some(cursor.x)
}

/// Whether a fullscreen app, game or presentation is in front, so the notch should
/// stay out of the way.
#[must_use]
pub fn fullscreen_app_in_front() -> bool {
    // SAFETY: SHQueryUserNotificationState has no preconditions.
    let state = unsafe { SHQueryUserNotificationState() };
    state.is_ok_and(|state| {
        matches!(
            state,
            QUNS_BUSY | QUNS_RUNNING_D3D_FULL_SCREEN | QUNS_PRESENTATION_MODE
        )
    })
}

/// A display connected to the PC.
#[derive(Debug, Clone, PartialEq)]
pub struct Monitor {
    /// Windows' device name, such as `\\.\DISPLAY1`; what the config stores.
    pub device: String,
    pub primary: bool,
    /// The whole display in physical pixels, taskbar included.
    pub bounds: RECT,
    handle: HMONITOR,
}

impl Monitor {
    /// How the settings window lists the display, e.g. "DISPLAY1 · 2560 × 1440 · primary".
    #[must_use]
    pub fn label(&self) -> String {
        let name = self.device.trim_start_matches(r"\\.\");
        let width = self.bounds.right - self.bounds.left;
        let height = self.bounds.bottom - self.bounds.top;
        let primary = if self.primary { " · primary" } else { "" };
        format!("{name} · {width} × {height}{primary}")
    }
}

/// Every display connected to the PC, in Windows' order.
#[must_use]
pub fn monitors() -> Vec<Monitor> {
    unsafe extern "system" fn collect(
        monitor: HMONITOR,
        _: HDC,
        _: *mut RECT,
        found: LPARAM,
    ) -> BOOL {
        // SAFETY: `found` is the `Vec` below, which outlives the enumeration.
        let found = unsafe { &mut *(found.0 as *mut Vec<HMONITOR>) };
        found.push(monitor);
        true.into()
    }
    let mut handles: Vec<HMONITOR> = Vec::new();
    // SAFETY: `collect` only runs during this call, while `handles` is alive and not
    // otherwise borrowed.
    let _ = unsafe {
        EnumDisplayMonitors(None, None, Some(collect), LPARAM(&raw mut handles as isize))
    };
    handles
        .into_iter()
        .filter_map(|handle| monitor_info(handle).ok())
        .collect()
}

fn monitor_info(handle: HMONITOR) -> anyhow::Result<Monitor> {
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = mem::size_of::<MONITORINFOEXW>() as u32;
    // SAFETY: `info` is a MONITORINFOEXW whose cbSize says so, and the pointer covers
    // the whole struct the call writes into.
    if !unsafe { GetMonitorInfoW(handle, (&raw mut info).cast::<MONITORINFO>()) }.as_bool() {
        bail!("GetMonitorInfoW failed for monitor {handle:?}");
    }
    let name_len = info
        .szDevice
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(info.szDevice.len());
    Ok(Monitor {
        device: String::from_utf16_lossy(&info.szDevice[..name_len]),
        primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
        bounds: info.monitorInfo.rcMonitor,
        handle,
    })
}

/// Where the notch sits: a display, and how far along its top edge the window's
/// center is, from 0 (left edge) to 1 (right edge).
#[derive(Debug, Clone, PartialEq)]
pub struct Placement {
    /// Windows' device name; the primary display when `None` or disconnected.
    pub device: Option<String>,
    pub position: f32,
}

impl Placement {
    /// Moves the window there, keeping the whole window on its display; the position
    /// is clamped to match.
    ///
    /// Before the first `show()` this keeps the window from appearing anywhere else first.
    /// `logical_width` is the window width in logical pixels, since Slint reports no size
    /// before the window exists.
    ///
    /// # Errors
    ///
    /// Fails when Windows reports no display, or not its DPI.
    pub fn apply(&mut self, window: &slint::Window, logical_width: f32) -> anyhow::Result<()> {
        self.place(window, logical_width, self.position, 0)
    }

    /// Moves the window `dx` physical pixels along the top edge from `start`, the
    /// position it had when a drag began.
    ///
    /// # Errors
    ///
    /// Fails when Windows reports no display, or not its DPI.
    pub fn drag(
        &mut self,
        window: &slint::Window,
        logical_width: f32,
        start: f32,
        dx: i32,
    ) -> anyhow::Result<()> {
        self.place(window, logical_width, start, dx)
    }

    fn place(
        &mut self,
        window: &slint::Window,
        logical_width: f32,
        position: f32,
        dx: i32,
    ) -> anyhow::Result<()> {
        let monitors = monitors();
        let monitor = monitors
            .iter()
            .find(|monitor| Some(monitor.device.as_str()) == self.device.as_deref())
            .or_else(|| monitors.iter().find(|monitor| monitor.primary))
            .context("Windows reports no display")?;
        let scale = monitor_scale(monitor.handle)?;
        let bounds = monitor.bounds;

        // Pixel counts are far inside f32's exact range.
        let (left, span) = (bounds.left as f32, (bounds.right - bounds.left) as f32);
        let half = logical_width * scale / 2.0;
        let lowest = left + half;
        let center =
            (left + position * span + dx as f32).clamp(lowest, (left + span - half).max(lowest));
        self.position = (center - left) / span;
        // Rounding to whole pixels is intended.
        let x = (center - half).round() as i32;
        window.set_position(slint::PhysicalPosition::new(x, bounds.top));
        Ok(())
    }
}

fn monitor_scale(monitor: HMONITOR) -> anyhow::Result<f32> {
    let (mut dpi_x, mut dpi_y) = (0u32, 0u32);
    // SAFETY: both out-pointers point at live u32 locals.
    unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) }
        .with_context(|| format!("GetDpiForMonitor failed for monitor {monitor:?}"))?;
    // DPI values are small integers, so the f32 conversion is exact.
    Ok(dpi_x as f32 / BASE_DPI)
}

/// The native window behind the Slint notch window.
#[derive(Debug)]
pub struct NotchWindow {
    hwnd: HWND,
}

impl NotchWindow {
    /// Finds the native handle of a Slint window.
    ///
    /// # Errors
    ///
    /// Fails until Slint has created the native window, which happens after the event
    /// loop starts, and for any non-Win32 window.
    pub fn attach(window: &slint::Window) -> anyhow::Result<Self> {
        let handle = window.window_handle();
        let raw = handle
            .window_handle()
            .context("slint has not created the native window yet")?
            .as_raw();
        let RawWindowHandle::Win32(win32) = raw else {
            bail!("expected a Win32 window handle, got {raw:?}");
        };
        let hwnd = HWND(win32.hwnd.get() as *mut c_void);
        Ok(Self { hwnd })
    }

    /// Removes the window from the taskbar and Alt+Tab, and stops clicks from stealing focus.
    pub fn apply_tool_styles(&self) {
        // SAFETY: `hwnd` is a live window owned by this thread's Slint event loop.
        unsafe {
            let style = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE);
            let style = (style & !(WS_EX_APPWINDOW.0 as isize))
                | WS_EX_TOOLWINDOW.0 as isize
                | WS_EX_NOACTIVATE.0 as isize;
            // The taskbar only re-reads the style when the window is shown again.
            let _ = ShowWindow(self.hwnd, SW_HIDE);
            SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, style);
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
    }

    /// Hides the window behind Slint's back: hiding Slint's last window would end the
    /// event loop.
    pub fn hide(&self) {
        // SAFETY: `hwnd` is a live window owned by this thread's Slint event loop.
        let _ = unsafe { ShowWindow(self.hwnd, SW_HIDE) };
    }

    /// Shows the window again after [`NotchWindow::hide`], without taking focus.
    pub fn show(&self) {
        // SAFETY: `hwnd` is a live window owned by this thread's Slint event loop.
        let _ = unsafe { ShowWindow(self.hwnd, SW_SHOWNOACTIVATE) };
    }

    /// Shows the right-click menu at the cursor, with `shown` marked as the provider on
    /// the notch, and waits for a pick.
    ///
    /// Returns `None` when the menu is dismissed or cannot be shown.
    pub fn show_menu(&self, shown: Provider) -> Option<MenuChoice> {
        // SAFETY: the menu is created, used and destroyed here; `hwnd` is live and owns
        // the menu while it tracks.
        let picked = unsafe {
            let menu = CreatePopupMenu().ok()?;
            let picked = fill_menu(menu, shown).ok().and_then(|()| {
                let mut cursor = POINT::default();
                GetCursorPos(&mut cursor).ok()?;
                // Without this the menu does not close when clicking elsewhere.
                let _ = SetForegroundWindow(self.hwnd);
                let flags = TPM_RETURNCMD | TPM_RIGHTBUTTON;
                let id = TrackPopupMenu(menu, flags, cursor.x, cursor.y, None, self.hwnd, None);
                usize::try_from(id.0).ok()
            });
            let _ = DestroyMenu(menu);
            picked?
        };
        match picked {
            MENU_QUIT => Some(MenuChoice::Quit),
            MENU_SETTINGS => Some(MenuChoice::Settings),
            id => id
                .checked_sub(MENU_FIRST_PROVIDER)
                .and_then(|index| Provider::ALL.get(index))
                .map(|&provider| MenuChoice::Show(provider)),
        }
    }

    /// Limits drawing and mouse input to `rect`; clicks outside it reach the windows below.
    pub fn clip_to(&self, rect: RECT) {
        // SAFETY: `hwnd` is live. On success the system owns the region; on failure we
        // still own it and delete it.
        unsafe {
            let region = CreateRectRgn(rect.left, rect.top, rect.right, rect.bottom);
            if SetWindowRgn(self.hwnd, Some(region), true) == 0 {
                let _ = DeleteObject(region.into());
            }
        }
    }
}

/// Adds the provider choices, with `shown` marked, and Quit to an empty popup menu.
///
/// # Safety
///
/// `menu` must be a live menu handle.
unsafe fn fill_menu(menu: HMENU, shown: Provider) -> windows::core::Result<()> {
    let id = |index: usize| MENU_FIRST_PROVIDER + index;
    let check_id = |index: usize| u32::try_from(id(index)).expect("menu ids are small");
    let shown = Provider::ALL
        .iter()
        .position(|&provider| provider == shown)
        .expect("Provider::ALL lists every provider");
    let last = Provider::ALL.len() - 1;
    // SAFETY: the caller guarantees `menu` is live; every string outlives its call.
    unsafe {
        for (index, provider) in Provider::ALL.iter().enumerate() {
            AppendMenuW(menu, MF_STRING, id(index), &HSTRING::from(provider.name()))?;
        }
        CheckMenuRadioItem(
            menu,
            check_id(0),
            check_id(last),
            check_id(shown),
            MF_BYCOMMAND.0,
        )?;
        AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null())?;
        AppendMenuW(menu, MF_STRING, MENU_SETTINGS, w!("Settings…"))?;
        AppendMenuW(menu, MF_STRING, MENU_QUIT, w!("Quit ai-notch"))
    }
}

/// The smallest rectangle holding both `a` and `b`.
#[must_use]
pub fn union(a: RECT, b: RECT) -> RECT {
    RECT {
        left: a.left.min(b.left),
        top: a.top.min(b.top),
        right: a.right.max(b.right),
        bottom: a.bottom.max(b.bottom),
    }
}

/// The top-centered rectangle, in physical pixels, that holds a shape of the given
/// logical size inside a window of the given logical width.
///
/// Rounds outward so anti-aliased edges are never cut off.
#[must_use]
pub fn shape_rect(window_width: f32, shape_width: f32, shape_height: f32, scale: f32) -> RECT {
    let left = (window_width - shape_width) / 2.0 * scale;
    let right = left + shape_width * scale;
    // Pixel coordinates are far inside i32 range; truncation after floor/ceil is intended.
    RECT {
        left: left.floor() as i32,
        top: 0,
        right: right.ceil() as i32,
        bottom: (shape_height * scale).ceil() as i32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn union_holds_both_rectangles() {
        let wide = RECT {
            left: 0,
            top: 0,
            right: 460,
            bottom: 30,
        };
        let tall = RECT {
            left: 100,
            top: 0,
            right: 360,
            bottom: 196,
        };
        let both = RECT {
            left: 0,
            top: 0,
            right: 460,
            bottom: 196,
        };
        assert_eq!(union(wide, tall), both);
        assert_eq!(union(both, wide), both);
    }

    #[test]
    fn there_is_one_primary_monitor() {
        let monitors = monitors();
        let primaries = monitors.iter().filter(|monitor| monitor.primary).count();
        assert_eq!(primaries, 1, "{monitors:?}");
        assert!(monitors.iter().all(|monitor| !monitor.device.is_empty()));
    }

    #[test]
    fn shape_rect_is_centered_at_100_percent() {
        let rect = shape_rect(520.0, 190.0, 30.0, 1.0);
        assert_eq!(
            (rect.left, rect.top, rect.right, rect.bottom),
            (165, 0, 355, 30)
        );
    }

    #[test]
    fn shape_rect_rounds_outward_when_scaled() {
        let rect = shape_rect(520.0, 191.0, 30.5, 1.25);
        // Unrounded: left 205.625, right 444.375, bottom 38.125.
        assert_eq!(
            (rect.left, rect.right, rect.bottom),
            (205, 445, 39),
            "{rect:?}"
        );
    }
}
