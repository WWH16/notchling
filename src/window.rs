//! Win32 window tweaks that Slint does not expose: tool-window styles and placement.

use std::ffi::c_void;
use std::mem;
use std::sync::OnceLock;

use anyhow::{Context, bail};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateRectRgn, DeleteObject, EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR,
    MONITOR_DEFAULTTONEAREST, MONITORINFO, MONITORINFOEXW, MonitorFromPoint, SetWindowRgn,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Shell::{
    QUNS_BUSY, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN, SHQueryUserNotificationState,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CallWindowProcW, CheckMenuRadioItem, CreatePopupMenu, CreateWindowExW,
    DestroyMenu, DestroyWindow, GWL_EXSTYLE, GWL_STYLE, GWLP_WNDPROC, GetCursorPos,
    GetWindowLongPtrW, HMENU, MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MF_BYCOMMAND, MF_SEPARATOR,
    MF_STRING, MONITORINFOF_PRIMARY, MessageBoxW, PostMessageW, SW_HIDE, SW_SHOWNOACTIVATE,
    SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetForegroundWindow,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu,
    WM_NCACTIVATE, WM_NCPAINT, WM_NULL, WNDPROC, WS_CAPTION, WS_EX_APPWINDOW, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_WINDOWEDGE, WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_POPUP, WS_SIZEBOX,
    WS_SYSMENU,
};
use windows::core::{BOOL, HSTRING, PCWSTR, w};

use crate::usage::Provider;

/// DPI that Windows treats as 100% scaling.
const BASE_DPI: f32 = 96.0;

/// winit's window procedure for the notch, which [`skip_frame_painting`] hands every
/// other message to.
static WINIT_WNDPROC: OnceLock<WNDPROC> = OnceLock::new();

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

/// The cursor's screen position in physical pixels, if Windows reports it.
#[must_use]
pub fn cursor() -> Option<POINT> {
    let mut cursor = POINT::default();
    // SAFETY: `cursor` is a live POINT the call writes into.
    unsafe { GetCursorPos(&mut cursor) }.ok()?;
    Some(cursor)
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
    /// The display without the taskbar and docked bars.
    pub work: RECT,
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
        work: info.monitorInfo.rcWork,
        handle,
    })
}

/// Where the notch hangs: from the top edge of its display, from the bottom of the work
/// area (just above the taskbar), or floating anywhere in between.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Top,
    Bottom,
    Floating,
}

/// Which way the notch expands from its fixed side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Growth {
    Down,
    Up,
}

/// How close to the top or bottom edge, in logical pixels, a dropped notch sticks to it.
const SNAP_DISTANCE: f32 = 32.0;

/// Where the notch sits. The anchor is the middle of its fixed side: the top while it
/// grows down, the bottom while it grows up.
#[derive(Debug, Clone, PartialEq)]
pub struct Placement {
    /// Windows' device name; the primary display when `None` or disconnected.
    pub device: Option<String>,
    /// The anchor across the display, from 0 (left edge) to 1 (right edge).
    pub position: f32,
    /// The anchor down the display, from 0 (top edge) to 1 (bottom of the work area).
    pub vertical: f32,
}

impl Placement {
    #[must_use]
    pub fn edge(&self) -> Edge {
        if self.vertical <= 0.0 {
            Edge::Top
        } else if self.vertical >= 1.0 {
            Edge::Bottom
        } else {
            Edge::Floating
        }
    }

    /// Up in the lower half of the display, so a tall request card stays on screen.
    #[must_use]
    pub fn growth(&self) -> Growth {
        growth(self.vertical)
    }

    /// Moves the window there, keeping it on its display; the position is clamped to
    /// match. `size` is the window size in logical pixels, since Slint reports no size
    /// before the window exists. Before the first `show()` this keeps the window from
    /// appearing anywhere else first.
    ///
    /// # Errors
    ///
    /// Fails when Windows reports no display, or not its DPI.
    pub fn apply(
        &mut self,
        window: &slint::Window,
        size: slint::LogicalSize,
    ) -> anyhow::Result<()> {
        let area = Area::of(&self.monitor()?)?;
        let (x, y, position) = area.origin(size, self.position, self.vertical);
        self.position = position;
        window.set_position(slint::PhysicalPosition::new(x, y));
        Ok(())
    }

    /// The anchor in screen pixels, where a drag starts from.
    ///
    /// # Errors
    ///
    /// Fails when Windows reports no display, or not its DPI.
    pub fn anchor(&self) -> anyhow::Result<POINT> {
        let (x, y) = Area::of(&self.monitor()?)?.anchor(self.position, self.vertical);
        // Rounding to whole pixels is intended.
        Ok(POINT {
            x: x.round() as i32,
            y: y.round() as i32,
        })
    }

    /// Puts the anchor at `point`, in screen pixels, on whichever display holds it, as a
    /// drag does.
    ///
    /// # Errors
    ///
    /// Fails when Windows reports no display, or not its DPI.
    pub fn move_anchor(
        &mut self,
        window: &slint::Window,
        size: slint::LogicalSize,
        point: POINT,
    ) -> anyhow::Result<()> {
        // SAFETY: MonitorFromPoint has no preconditions; with DEFAULTTONEAREST it always
        // returns a display.
        let monitor = monitor_info(unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST) })?;
        let area = Area::of(&monitor)?;
        // Pixel counts are far inside f32's exact range.
        (self.position, self.vertical) = area.fractions(point.x as f32, point.y as f32);
        self.device = Some(monitor.device);
        self.apply(window, size)
    }

    /// Sticks the notch to the top or bottom edge when it was let go within
    /// [`SNAP_DISTANCE`] of one; anywhere else it stays where it is.
    ///
    /// # Errors
    ///
    /// Fails when Windows reports no display, or not its DPI.
    pub fn snap(&mut self, window: &slint::Window, size: slint::LogicalSize) -> anyhow::Result<()> {
        self.vertical = Area::of(&self.monitor()?)?.snapped(self.vertical);
        self.apply(window, size)
    }

    fn monitor(&self) -> anyhow::Result<Monitor> {
        let mut monitors = monitors();
        let index = monitors
            .iter()
            .position(|monitor| Some(monitor.device.as_str()) == self.device.as_deref())
            .or_else(|| monitors.iter().position(|monitor| monitor.primary))
            .context("Windows reports no display")?;
        Ok(monitors.swap_remove(index))
    }
}

fn growth(vertical: f32) -> Growth {
    if vertical > 0.5 {
        Growth::Up
    } else {
        Growth::Down
    }
}

/// A display as the notch uses it, in physical pixels: the whole width, from the top of
/// the display to the bottom of its work area.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Area {
    left: f32,
    right: f32,
    top: f32,
    bottom: f32,
    scale: f32,
}

impl Area {
    fn of(monitor: &Monitor) -> anyhow::Result<Self> {
        // Pixel counts are far inside f32's exact range.
        Ok(Self {
            left: monitor.bounds.left as f32,
            right: monitor.bounds.right as f32,
            top: monitor.bounds.top as f32,
            bottom: monitor.work.bottom as f32,
            scale: monitor_scale(monitor.handle)?,
        })
    }

    fn anchor(self, position: f32, vertical: f32) -> (f32, f32) {
        (
            self.left + position * (self.right - self.left),
            self.top + vertical * (self.bottom - self.top),
        )
    }

    fn fractions(self, x: f32, y: f32) -> (f32, f32) {
        (
            ((x - self.left) / (self.right - self.left)).clamp(0.0, 1.0),
            ((y - self.top) / (self.bottom - self.top)).clamp(0.0, 1.0),
        )
    }

    /// The window's top-left corner for a placement, keeping the whole window across
    /// the display, and the position that leaves.
    fn origin(self, size: slint::LogicalSize, position: f32, vertical: f32) -> (i32, i32, f32) {
        let (width, height) = (size.width * self.scale, size.height * self.scale);
        let half = width / 2.0;
        let (x, y) = self.anchor(position, vertical);
        let lowest = self.left + half;
        let x = x.clamp(lowest, (self.right - half).max(lowest));
        let top = match growth(vertical) {
            Growth::Down => y,
            Growth::Up => y - height,
        };
        let position = (x - self.left) / (self.right - self.left);
        // Rounding to whole pixels is intended.
        ((x - half).round() as i32, top.round() as i32, position)
    }

    /// `vertical` pulled onto the top or bottom edge when it is within
    /// [`SNAP_DISTANCE`] of one.
    fn snapped(self, vertical: f32) -> f32 {
        let span = self.bottom - self.top;
        let distance = SNAP_DISTANCE * self.scale;
        if vertical * span <= distance {
            0.0
        } else if (1.0 - vertical) * span <= distance {
            1.0
        } else {
            vertical
        }
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

    /// Removes the window from the taskbar and Alt+Tab, stops clicks from stealing focus,
    /// and drops the caption and frame styles.
    ///
    /// winit keeps a caption on frameless windows and hides it in `WM_NCCALCSIZE`. Once
    /// the window has a region, Windows paints that caption over the notch whenever the
    /// window is activated, as it is for the right-click menu; a plain popup has none.
    pub fn apply_tool_styles(&self) {
        let frame = WS_CAPTION | WS_SYSMENU | WS_SIZEBOX | WS_MINIMIZEBOX | WS_MAXIMIZEBOX;
        // SAFETY: `hwnd` is a live window owned by this thread's Slint event loop.
        unsafe {
            let style = GetWindowLongPtrW(self.hwnd, GWL_STYLE);
            let style = (style & !(frame.0 as isize)) | WS_POPUP.0 as isize;
            let ex_style = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE);
            let ex_style = (ex_style & !((WS_EX_APPWINDOW | WS_EX_WINDOWEDGE).0 as isize))
                | WS_EX_TOOLWINDOW.0 as isize
                | WS_EX_NOACTIVATE.0 as isize;
            // The taskbar only re-reads the style when the window is shown again.
            let _ = ShowWindow(self.hwnd, SW_HIDE);
            SetWindowLongPtrW(self.hwnd, GWL_STYLE, style);
            SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, ex_style);
            // Style changes to the frame apply only after this.
            let _ = SetWindowPos(
                self.hwnd,
                None,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            );
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
        self.skip_frame_painting();
    }

    /// Puts [`skip_frame_painting`] in front of winit's window procedure. Even without
    /// caption styles, Windows paints a title bar over a window that has a region when
    /// it is activated or deactivated, which clicking the notch and opening its menu do.
    fn skip_frame_painting(&self) {
        // SAFETY: `hwnd` is a live window of this thread. The previous procedure is saved
        // before the new one can run, and the new one only forwards to it.
        unsafe {
            let ours = skip_frame_painting as unsafe extern "system" fn(_, _, _, _) -> _;
            let previous = SetWindowLongPtrW(self.hwnd, GWLP_WNDPROC, ours as usize as isize);
            // SAFETY: a window procedure address read back as an integer is that
            // procedure; 0 would mean none, which `WNDPROC` models as `None`.
            let previous = mem::transmute::<isize, WNDPROC>(previous);
            if WINIT_WNDPROC.set(previous).is_err() {
                // Installed twice; put the first one back rather than forward to ourselves.
                SetWindowLongPtrW(
                    self.hwnd,
                    GWLP_WNDPROC,
                    previous.map_or(0, |p| p as usize as isize),
                );
            }
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

/// The notch's window procedure: drops the frame painting Windows does on activation,
/// and passes everything else to winit.
unsafe extern "system" fn skip_frame_painting(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        // Activation still changes; only the repaint is skipped. TRUE lets it deactivate.
        WM_NCACTIVATE => LRESULT(1),
        // The notch has no frame to paint.
        WM_NCPAINT => LRESULT(0),
        _ => {
            let winit = WINIT_WNDPROC.get().copied().flatten();
            // SAFETY: `winit` is the procedure this window had before ours, and these are
            // the arguments Windows passed us for it.
            unsafe { CallWindowProcW(winit, hwnd, message, wparam, lparam) }
        }
    }
}

/// Shows the right-click menu at the cursor, with `shown` marked as the provider on the
/// notch, and waits for a pick.
///
/// The menu belongs to a hidden helper window, not the notch. A menu's owner has to be
/// the foreground window, and activating the notch makes Windows paint a caption over
/// it, whatever its styles say.
///
/// Returns `None` when the menu is dismissed or cannot be shown.
#[must_use]
pub fn show_menu(shown: Provider) -> Option<MenuChoice> {
    // SAFETY: the helper window and the menu are created, used and destroyed here on
    // this thread; the helper owns the menu while it tracks.
    let picked = unsafe {
        let owner = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            w!("STATIC"),
            PCWSTR::null(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            None,
            None,
        )
        .ok()?;
        let picked = CreatePopupMenu().ok().and_then(|menu| {
            let picked = fill_menu(menu, shown).ok().and_then(|()| {
                let mut cursor = POINT::default();
                GetCursorPos(&mut cursor).ok()?;
                // Without this the menu does not close when clicking elsewhere.
                let _ = SetForegroundWindow(owner);
                let flags = TPM_RETURNCMD | TPM_RIGHTBUTTON;
                let id = TrackPopupMenu(menu, flags, cursor.x, cursor.y, None, owner, None);
                // The TrackPopupMenu docs advise this, so the next menu closes properly.
                let _ = PostMessageW(Some(owner), WM_NULL, WPARAM(0), LPARAM(0));
                usize::try_from(id.0).ok()
            });
            let _ = DestroyMenu(menu);
            picked
        });
        let _ = DestroyWindow(owner);
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

/// The rectangle, in physical pixels, that holds a shape of the given logical size
/// inside a window of the given logical size: centered across it, against the top when
/// the notch grows down and against the bottom when it grows up.
///
/// Rounds outward so anti-aliased edges are never cut off.
#[must_use]
pub fn shape_rect(
    window: slint::LogicalSize,
    shape: slint::LogicalSize,
    scale: f32,
    growth: Growth,
) -> RECT {
    let left = (window.width - shape.width) / 2.0 * scale;
    let right = left + shape.width * scale;
    let (top, bottom) = match growth {
        Growth::Down => (0.0, shape.height * scale),
        Growth::Up => (
            (window.height - shape.height) * scale,
            window.height * scale,
        ),
    };
    // Pixel coordinates are far inside i32 range; truncation after floor/ceil is intended.
    RECT {
        left: left.floor() as i32,
        top: top.floor() as i32,
        right: right.ceil() as i32,
        bottom: bottom.ceil() as i32,
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
        let window = slint::LogicalSize::new(520.0, 280.0);
        let shape = slint::LogicalSize::new(190.0, 30.0);
        let down = shape_rect(window, shape, 1.0, Growth::Down);
        assert_eq!(
            (down.left, down.top, down.right, down.bottom),
            (165, 0, 355, 30)
        );
        let up = shape_rect(window, shape, 1.0, Growth::Up);
        assert_eq!((up.left, up.top, up.right, up.bottom), (165, 250, 355, 280));
    }

    #[test]
    fn shape_rect_rounds_outward_when_scaled() {
        let window = slint::LogicalSize::new(520.0, 280.0);
        let rect = shape_rect(
            window,
            slint::LogicalSize::new(191.0, 30.5),
            1.25,
            Growth::Down,
        );
        // Unrounded: left 205.625, right 444.375, bottom 38.125.
        assert_eq!(
            (rect.left, rect.right, rect.bottom),
            (205, 445, 39),
            "{rect:?}"
        );
    }

    /// A 1920x1080 display at 125% with a 48-pixel taskbar.
    fn area() -> Area {
        Area {
            left: 0.0,
            right: 1920.0,
            top: 0.0,
            bottom: 1032.0,
            scale: 1.25,
        }
    }

    #[test]
    fn notch_hangs_from_the_edge_it_is_on() {
        let size = slint::LogicalSize::new(520.0, 280.0);
        // 650 x 350 physical pixels.
        assert_eq!(area().origin(size, 0.5, 0.0), (635, 0, 0.5), "top edge");
        assert_eq!(
            area().origin(size, 0.5, 1.0),
            (635, 682, 0.5),
            "bottom edge grows up"
        );
        assert_eq!(
            area().origin(size, 0.5, 0.25),
            (635, 258, 0.5),
            "upper half grows down"
        );
        let (x, _, position) = area().origin(size, 0.0, 0.0);
        assert_eq!((x, position), (0, 325.0 / 1920.0), "kept on the display");
    }

    #[test]
    fn dropped_notch_sticks_only_near_an_edge() {
        // 32 logical pixels are 40 physical ones; the area is 1032 tall.
        assert_eq!(area().snapped(39.0 / 1032.0), 0.0);
        assert_eq!(area().snapped(1.0 - 39.0 / 1032.0), 1.0);
        let middle = 500.0 / 1032.0;
        assert_eq!(area().snapped(middle), middle);
        assert_eq!(area().snapped(41.0 / 1032.0), 41.0 / 1032.0);
    }

    #[test]
    fn anchor_and_fractions_agree() {
        let (x, y) = area().anchor(0.25, 0.75);
        assert_eq!(area().fractions(x, y), (0.25, 0.75));
        assert_eq!(
            area().fractions(-50.0, 5000.0),
            (0.0, 1.0),
            "clamped to the display"
        );
    }
}
