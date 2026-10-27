//! Win32 window tweaks that Slint does not expose: tool-window styles and placement.

use std::cell::RefCell;
use std::env;
use std::ffi::c_void;
use std::mem;
use std::path::PathBuf;
use std::sync::OnceLock;

use anyhow::{Context, bail};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateRectRgn, DeleteObject, EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR,
    MONITOR_DEFAULTTONEAREST, MONITORINFO, MONITORINFOEXW, MonitorFromPoint, SetWindowRgn,
};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey, VIRTUAL_KEY, VK_N, VK_Y,
};
use windows::Win32::UI::Shell::{
    QUNS_BUSY, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN, SHQueryUserNotificationState,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CallWindowProcW, CheckMenuRadioItem, CreatePopupMenu, CreateWindowExW,
    DestroyMenu, DestroyWindow, GWL_EXSTYLE, GWL_STYLE, GWLP_WNDPROC, GetCursorPos,
    GetSystemMetrics, GetWindowLongPtrW, HMENU, IsWindowVisible, MB_ICONERROR, MB_ICONINFORMATION,
    MB_OK, MF_BYCOMMAND, MF_SEPARATOR, MF_STRING, MONITORINFOF_PRIMARY, MessageBoxW, PostMessageW,
    SM_CXDRAG, SM_CYDRAG, SPI_GETCLIENTAREAANIMATION, SW_HIDE, SW_SHOWNOACTIVATE, SWP_FRAMECHANGED,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    SetForegroundWindow, SetWindowLongPtrW, SetWindowPos, ShowWindow, SystemParametersInfoW,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, WM_HOTKEY, WM_NCACTIVATE, WM_NCPAINT, WM_NULL,
    WNDPROC, WS_CAPTION, WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_WINDOWEDGE,
    WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_POPUP, WS_SIZEBOX, WS_SYSMENU,
};
use windows::core::{BOOL, HSTRING, PCWSTR, w};

use crate::usage::Provider;

/// DPI that Windows treats as 100% scaling.
const BASE_DPI: f32 = 96.0;

/// The largest text size Windows offers, 225%.
const MAX_TEXT_SCALE: f32 = 2.25;

/// winit's window procedure for the notch, which [`notch_procedure`] hands every other
/// message to.
static WINIT_WNDPROC: OnceLock<WNDPROC> = OnceLock::new();

/// What the notch does when one of its [`Shortcut`]s is pressed.
type ShortcutHandler = Box<dyn Fn(Shortcut)>;

thread_local! {
    static ON_SHORTCUT: RefCell<Option<ShortcutHandler>> = const { RefCell::new(None) };
}

/// A global shortcut that answers the permission request on the notch: Ctrl+Alt and a
/// letter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shortcut {
    /// Ctrl+Alt+Y.
    Allow,
    /// Ctrl+Alt+N.
    Deny,
}

impl Shortcut {
    const ALL: [Self; 2] = [Self::Allow, Self::Deny];

    /// The hotkey id, unique within the notch window.
    fn id(self) -> i32 {
        match self {
            Self::Allow => 1,
            Self::Deny => 2,
        }
    }

    fn key(self) -> VIRTUAL_KEY {
        match self {
            Self::Allow => VK_Y,
            Self::Deny => VK_N,
        }
    }

    fn from_id(id: usize) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|shortcut| usize::try_from(shortcut.id()) == Ok(id))
    }
}

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

/// Whether the cursor has moved from `start` to `now` far enough to start a drag, by
/// Windows' own threshold, so a click with a shaky hand stays a click.
#[must_use]
pub fn past_drag_threshold(start: POINT, now: POINT) -> bool {
    // SAFETY: GetSystemMetrics has no preconditions.
    let (width, height) = unsafe { (GetSystemMetrics(SM_CXDRAG), GetSystemMetrics(SM_CYDRAG)) };
    (now.x - start.x).abs() >= width || (now.y - start.y).abs() >= height
}

/// Whether Windows' "Show animations" setting is on. Assumed on when it cannot be read.
#[must_use]
pub fn animations_enabled() -> bool {
    let mut enabled = BOOL(1);
    // SAFETY: SPI_GETCLIENTAREAANIMATION writes one BOOL into `enabled`, which outlives
    // the call.
    let _ = unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            Some((&raw mut enabled).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    enabled.as_bool()
}

/// Windows' text size (Settings, Accessibility, Text size) as a factor, 1 at 100%.
#[must_use]
pub fn text_scale() -> f32 {
    let mut percent = 100u32;
    let mut size = mem::size_of::<u32>() as u32;
    // SAFETY: the key and value names are static wide strings; RRF_RT_REG_DWORD makes the
    // call write at most `size` bytes, one u32, into `percent`, which outlives it.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"SOFTWARE\Microsoft\Accessibility"),
            w!("TextScaleFactor"),
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut percent).cast()),
            Some(&mut size),
        )
    };
    // Unset until the user first changes it, which means 100%.
    if status.is_err() {
        return 1.0;
    }
    // A percentage is far inside f32's exact range.
    (percent as f32 / 100.0).clamp(1.0, MAX_TEXT_SCALE)
}

/// The monospaced font for commands: Cascadia Mono where it is installed for everyone
/// or for this user, else Consolas, which every Windows has.
#[must_use]
pub fn mono_font() -> &'static str {
    let font_dirs = [
        env::var_os("WINDIR").map(|dir| PathBuf::from(dir).join("Fonts")),
        env::var_os("LOCALAPPDATA").map(|dir| PathBuf::from(dir).join(r"Microsoft\Windows\Fonts")),
    ];
    let installed = font_dirs
        .into_iter()
        .flatten()
        .any(|dir| dir.join("CascadiaMono.ttf").exists());
    if installed {
        "Cascadia Mono"
    } else {
        "Consolas"
    }
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

/// The screen edge the notch sticks to, flat side against it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Edge {
    #[default]
    Top,
    Bottom,
    Left,
    Right,
}

/// Where the notch sits: a display, the edge it sticks to, and how far along that edge
/// its middle is, from 0 to 1 (left to right on the top and bottom edges, top to
/// bottom on the side edges).
#[derive(Debug, Clone, PartialEq)]
pub struct Placement {
    /// Windows' device name; the primary display when `None` or disconnected.
    pub device: Option<String>,
    pub edge: Edge,
    pub position: f32,
}

impl Placement {
    /// Moves the window there at once. Before the first `show()` this keeps the window
    /// from appearing anywhere else first.
    ///
    /// # Errors
    ///
    /// Fails when Windows reports no display, or not its DPI.
    pub fn apply(
        &mut self,
        window: &slint::Window,
        size: slint::LogicalSize,
    ) -> anyhow::Result<()> {
        window.set_position(self.origin(size)?);
        Ok(())
    }

    /// The window's top-left corner for this placement, keeping the window on its
    /// display; the position is clamped to match. `size` is the window size in logical
    /// pixels, since Slint reports no size before the window exists.
    ///
    /// # Errors
    ///
    /// Fails when Windows reports no display, or not its DPI.
    pub fn origin(&mut self, size: slint::LogicalSize) -> anyhow::Result<slint::PhysicalPosition> {
        let area = Area::of(&self.monitor()?)?;
        let (x, y, position) = area.origin(size, self.edge, self.position);
        self.position = position;
        Ok(slint::PhysicalPosition::new(x, y))
    }

    /// Sticks the notch to whichever edge of whichever display is nearest `point`, in
    /// screen pixels, at the spot along that edge closest to it; where a drag lets go.
    /// Returns where the window belongs, leaving the move to the caller.
    ///
    /// # Errors
    ///
    /// Fails when Windows reports no display, or not its DPI.
    pub fn stick_near(
        &mut self,
        size: slint::LogicalSize,
        point: POINT,
    ) -> anyhow::Result<slint::PhysicalPosition> {
        // SAFETY: MonitorFromPoint has no preconditions; with DEFAULTTONEAREST it always
        // returns a display.
        let monitor = monitor_info(unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST) })?;
        // Pixel counts are far inside f32's exact range.
        (self.edge, self.position) = Area::of(&monitor)?.nearest(point.x as f32, point.y as f32);
        self.device = Some(monitor.device);
        self.origin(size)
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

/// A display as the notch uses it, in physical pixels: from the top of the display down
/// to the bottom of its work area, and across its work area, so the notch never covers a
/// taskbar.
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
            left: monitor.work.left as f32,
            right: monitor.work.right as f32,
            top: monitor.bounds.top as f32,
            bottom: monitor.work.bottom as f32,
            scale: monitor_scale(monitor.handle)?,
        })
    }

    /// The window's top-left corner for a placement, keeping the whole window on the
    /// display, and the position that leaves.
    fn origin(self, size: slint::LogicalSize, edge: Edge, position: f32) -> (i32, i32, f32) {
        let (width, height) = (size.width * self.scale, size.height * self.scale);
        // The window's middle along the edge, kept far enough in for the whole window.
        let along = |start: f32, end: f32, length: f32| {
            let half = length / 2.0;
            let middle = (start + position * (end - start))
                .clamp(start + half, (end - half).max(start + half));
            (middle - half, (middle - start) / (end - start))
        };
        let (x, y, position) = match edge {
            Edge::Top | Edge::Bottom => {
                let (x, position) = along(self.left, self.right, width);
                let y = if edge == Edge::Top {
                    self.top
                } else {
                    self.bottom - height
                };
                (x, y, position)
            }
            Edge::Left | Edge::Right => {
                let (y, position) = along(self.top, self.bottom, height);
                let x = if edge == Edge::Left {
                    self.left
                } else {
                    self.right - width
                };
                (x, y, position)
            }
        };
        // Rounding to whole pixels is intended.
        (x.round() as i32, y.round() as i32, position)
    }

    /// The edge nearest `(x, y)` and the spot along it closest to that point.
    fn nearest(self, x: f32, y: f32) -> (Edge, f32) {
        let distances = [
            (Edge::Top, y - self.top),
            (Edge::Bottom, self.bottom - y),
            (Edge::Left, x - self.left),
            (Edge::Right, self.right - x),
        ];
        let (edge, _) = distances
            .into_iter()
            .min_by(|(_, a), (_, b)| a.total_cmp(b))
            .expect("there are four edges");
        let position = match edge {
            Edge::Top | Edge::Bottom => (x - self.left) / (self.right - self.left),
            Edge::Left | Edge::Right => (y - self.top) / (self.bottom - self.top),
        };
        (edge, position.clamp(0.0, 1.0))
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
        self.take_over_procedure();
    }

    /// Puts [`notch_procedure`] in front of winit's window procedure.
    fn take_over_procedure(&self) {
        // SAFETY: `hwnd` is a live window of this thread. The previous procedure is saved
        // before the new one can run, and the new one only forwards to it.
        unsafe {
            let ours = notch_procedure as unsafe extern "system" fn(_, _, _, _) -> _;
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

    /// Runs `handler` on the UI thread whenever a claimed [`Shortcut`] is pressed while the
    /// notch is visible. It runs inside the window procedure, so it should defer any work
    /// that changes the window.
    pub fn on_shortcut(&self, handler: impl Fn(Shortcut) + 'static) {
        ON_SHORTCUT.with_borrow_mut(|slot| *slot = Some(Box::new(handler)));
    }

    /// Claims the [`Shortcut`]s from every other app. A key another app already holds
    /// stays with it.
    pub fn claim_shortcuts(&self) {
        for shortcut in Shortcut::ALL {
            let modifiers = MOD_CONTROL | MOD_ALT | MOD_NOREPEAT;
            // SAFETY: `hwnd` is a live window of this thread, which receives WM_HOTKEY.
            let _ = unsafe {
                RegisterHotKey(
                    Some(self.hwnd),
                    shortcut.id(),
                    modifiers,
                    shortcut.key().0.into(),
                )
            };
        }
    }

    /// Gives the [`Shortcut`]s back to other apps; harmless when they were not claimed.
    pub fn release_shortcuts(&self) {
        for shortcut in Shortcut::ALL {
            // SAFETY: `hwnd` is a live window of this thread.
            let _ = unsafe { UnregisterHotKey(Some(self.hwnd), shortcut.id()) };
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

/// The notch's window procedure: drops the frame painting Windows does on activation,
/// answers [`Shortcut`]s, and passes everything else to winit.
///
/// Even without caption styles, Windows paints a title bar over a window that has a
/// region when it is activated or deactivated, which clicking the notch and opening its
/// menu do.
unsafe extern "system" fn notch_procedure(
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
        WM_HOTKEY => {
            // SAFETY: `hwnd` is the live window this procedure belongs to.
            let visible = unsafe { IsWindowVisible(hwnd) }.as_bool();
            // A notch hidden behind a fullscreen app shows no request to answer.
            if let (true, Some(shortcut)) = (visible, Shortcut::from_id(wparam.0)) {
                ON_SHORTCUT.with_borrow(|handler| {
                    if let Some(handler) = handler {
                        handler(shortcut);
                    }
                });
            }
            LRESULT(0)
        }
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

/// The middle of `rect`.
#[must_use]
pub fn center(rect: RECT) -> POINT {
    POINT {
        x: (rect.left + rect.right) / 2,
        y: (rect.top + rect.bottom) / 2,
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
/// inside a window of the given logical size: against the window side that faces
/// `edge`, and centered the other way.
///
/// Rounds outward so anti-aliased edges are never cut off.
#[must_use]
pub fn shape_rect(
    window: slint::LogicalSize,
    shape: slint::LogicalSize,
    scale: f32,
    edge: Edge,
) -> RECT {
    let centered = |window: f32, shape: f32| (window - shape) / 2.0;
    let left = match edge {
        Edge::Top | Edge::Bottom => centered(window.width, shape.width),
        Edge::Left => 0.0,
        Edge::Right => window.width - shape.width,
    };
    let top = match edge {
        Edge::Left | Edge::Right => centered(window.height, shape.height),
        Edge::Top => 0.0,
        Edge::Bottom => window.height - shape.height,
    };
    // Pixel coordinates are far inside i32 range; truncation after floor/ceil is intended.
    RECT {
        left: (left * scale).floor() as i32,
        top: (top * scale).floor() as i32,
        right: ((left + shape.width) * scale).ceil() as i32,
        bottom: ((top + shape.height) * scale).ceil() as i32,
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
    fn shape_rect_sits_against_its_edge() {
        let window = slint::LogicalSize::new(520.0, 280.0);
        let shape = slint::LogicalSize::new(190.0, 30.0);
        let at = |edge| {
            let rect = shape_rect(window, shape, 1.0, edge);
            (rect.left, rect.top, rect.right, rect.bottom)
        };
        assert_eq!(at(Edge::Top), (165, 0, 355, 30));
        assert_eq!(at(Edge::Bottom), (165, 250, 355, 280));
        assert_eq!(at(Edge::Left), (0, 125, 190, 155));
        assert_eq!(at(Edge::Right), (330, 125, 520, 155));
    }

    #[test]
    fn shape_rect_rounds_outward_when_scaled() {
        let window = slint::LogicalSize::new(520.0, 280.0);
        let rect = shape_rect(
            window,
            slint::LogicalSize::new(191.0, 30.5),
            1.25,
            Edge::Top,
        );
        // Unrounded: left 205.625, right 444.375, bottom 38.125.
        assert_eq!(
            (rect.left, rect.right, rect.bottom),
            (205, 445, 39),
            "{rect:?}"
        );
    }

    /// A 1920x1080 display at 125% with a 48-pixel taskbar along the bottom.
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
    fn notch_sits_against_its_edge() {
        // 520 x 280 logical is 650 x 350 physical.
        let size = slint::LogicalSize::new(520.0, 280.0);
        assert_eq!(area().origin(size, Edge::Top, 0.5), (635, 0, 0.5));
        assert_eq!(area().origin(size, Edge::Bottom, 0.5), (635, 682, 0.5));
        assert_eq!(area().origin(size, Edge::Left, 0.5), (0, 341, 0.5));
        assert_eq!(area().origin(size, Edge::Right, 0.5), (1270, 341, 0.5));
        let (x, _, position) = area().origin(size, Edge::Top, 0.0);
        assert_eq!((x, position), (0, 325.0 / 1920.0), "kept on the display");
    }

    #[test]
    fn dropped_notch_sticks_to_the_nearest_edge() {
        assert_eq!(area().nearest(960.0, 100.0), (Edge::Top, 0.5));
        assert_eq!(area().nearest(960.0, 1000.0), (Edge::Bottom, 0.5));
        assert_eq!(area().nearest(50.0, 516.0), (Edge::Left, 0.5));
        assert_eq!(area().nearest(1900.0, 258.0), (Edge::Right, 0.25));
        let (edge, _) = area().nearest(-40.0, 5000.0);
        assert_eq!(
            edge,
            Edge::Bottom,
            "points off the display still find an edge"
        );
    }
}
