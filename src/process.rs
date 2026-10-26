//! Process-tree lookups used to find the window a Claude Code session runs in.

use std::collections::HashMap;
use std::mem;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::process;

use windows::Win32::Foundation::{HANDLE, HWND, LPARAM};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GetWindow, GetWindowTextLengthW, GetWindowThreadProcessId, IsIconic,
    IsWindowVisible, SW_RESTORE, SetForegroundWindow, ShowWindow,
};
use windows::core::BOOL;

/// How far up the tree to walk. Claude Code sits a few levels above the hook, and
/// the terminal or desktop app a few more.
const MAX_ANCESTORS: usize = 16;

/// Process ids from this process's parent upward, nearest first. Empty when the
/// process table cannot be read.
#[must_use]
pub fn ancestors() -> Vec<u32> {
    let parents = parent_map().unwrap_or_default();
    ancestors_of(process::id(), &parents)
}

/// Walks `parents` upward from `pid`, stopping at the root, a cycle, or the limit.
fn ancestors_of(pid: u32, parents: &HashMap<u32, u32>) -> Vec<u32> {
    let mut chain = Vec::new();
    let mut current = pid;
    while let Some(&parent) = parents.get(&current) {
        // Pid 0 is the idle process; a repeat means a reused pid formed a loop.
        if parent == 0 || parent == pid || chain.contains(&parent) || chain.len() == MAX_ANCESTORS {
            break;
        }
        chain.push(parent);
        current = parent;
    }
    chain
}

/// Brings the window of the nearest process in `pids` to the front, e.g. the terminal
/// or desktop app a Claude Code session runs in. Returns whether a window was found.
pub fn focus_first_window(pids: &[u32]) -> bool {
    let windows = app_windows();
    let Some(&(hwnd, _)) = pids
        .iter()
        .find_map(|pid| windows.iter().find(|(_, owner)| owner == pid))
    else {
        return false;
    };
    // SAFETY: `hwnd` came from EnumWindows a moment ago; a window that closed since
    // only makes these calls fail, which is harmless.
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        SetForegroundWindow(hwnd).as_bool()
    }
}

/// Visible, titled, unowned top-level windows with their process ids, front to back.
fn app_windows() -> Vec<(HWND, u32)> {
    unsafe extern "system" fn collect(hwnd: HWND, found: LPARAM) -> BOOL {
        // SAFETY: `found` is the Vec passed to EnumWindows below, alive for the call.
        let found = unsafe { &mut *(found.0 as *mut Vec<(HWND, u32)>) };
        // SAFETY: EnumWindows passes live top-level window handles.
        unsafe {
            let is_app_window = IsWindowVisible(hwnd).as_bool()
                && GetWindowTextLengthW(hwnd) > 0
                && GetWindow(hwnd, GW_OWNER).is_err();
            if is_app_window {
                let mut pid = 0;
                GetWindowThreadProcessId(hwnd, Some(&mut pid));
                found.push((hwnd, pid));
            }
        }
        true.into()
    }

    let mut found: Vec<(HWND, u32)> = Vec::new();
    // SAFETY: the callback only touches `found` through the pointer, during the call.
    let _ = unsafe { EnumWindows(Some(collect), LPARAM(&raw mut found as isize)) };
    found
}

/// Every running process's parent id, keyed by process id.
fn parent_map() -> windows::core::Result<HashMap<u32, u32>> {
    // SAFETY: the snapshot handle is owned by `OwnedHandle` and closed on drop; the
    // entry struct is correctly sized for each call.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)?;
        let snapshot = OwnedHandle::from_raw_handle(snapshot.0);
        let snapshot = HANDLE(snapshot.as_raw_handle());

        let mut entry = PROCESSENTRY32W {
            dwSize: mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut parents = HashMap::new();
        Process32FirstW(snapshot, &mut entry)?;
        loop {
            parents.insert(entry.th32ProcessID, entry.th32ParentProcessID);
            if Process32NextW(snapshot, &mut entry).is_err() {
                break;
            }
        }
        Ok(parents)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walks_up_to_the_root() {
        let parents = HashMap::from([(5, 4), (4, 3), (3, 0)]);
        assert_eq!(ancestors_of(5, &parents), vec![4, 3]);
    }

    #[test]
    fn stops_on_cycles() {
        let parents = HashMap::from([(5, 4), (4, 3), (3, 4)]);
        assert_eq!(ancestors_of(5, &parents), vec![4, 3]);
    }

    #[test]
    fn live_parent_is_first() {
        let chain = ancestors();
        assert!(!chain.is_empty(), "the test runner has a parent");
        assert!(!chain.contains(&process::id()), "{chain:?}");
    }
}
