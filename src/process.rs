//! Process-tree lookups used to find the window a Claude Code session runs in.

use std::collections::HashMap;
use std::mem;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::process;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};

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
