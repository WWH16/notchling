//! Named pipe between hook processes and the notch, open to the current user only.
//!
//! The notch owns the pipe name and accepts one connection per hook process. Each
//! connection is a plain [`File`], so the [`crate::protocol`] line helpers work on it.

use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io;
use std::mem;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};

use anyhow::{Context, bail};
use windows::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, HANDLE, HLOCAL, LocalFree,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT, PeekNamedPipe, WaitNamedPipeW,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::{HSTRING, PWSTR};

/// Pipe buffer size in each direction; messages are single short JSON lines.
const BUFFER_BYTES: u32 = 64 * 1024;

/// How many times a client retries while every pipe instance is busy.
const CONNECT_ATTEMPTS: u32 = 3;

/// How long a client waits for a free pipe instance per attempt.
const BUSY_WAIT_MS: u32 = 500;

/// The notch's pipe name for the current Windows user, e.g.
/// `\\.\pipe\ai-notch-S-1-5-21-...`.
///
/// # Errors
///
/// Fails when the current user's SID cannot be read.
pub fn pipe_name() -> anyhow::Result<String> {
    Ok(format!(r"\\.\pipe\ai-notch-{}", current_user_sid()?))
}

/// The current user's SID as a string, e.g. `S-1-5-21-...`.
fn current_user_sid() -> anyhow::Result<String> {
    // SAFETY: the token handle is owned and closed by `OwnedHandle`; the buffer is
    // u64-aligned, large enough per the size query, and outlives the SID pointer; the
    // string from ConvertSidToStringSidW is freed with LocalFree exactly once.
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)
            .context("OpenProcessToken failed")?;
        let token = OwnedHandle::from_raw_handle(token.0);
        let token = HANDLE(token.as_raw_handle());

        let mut len = 0u32;
        // The size query always "fails" with ERROR_INSUFFICIENT_BUFFER.
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
        let mut buffer = vec![0u64; (len as usize).div_ceil(mem::size_of::<u64>())];
        GetTokenInformation(
            token,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            len,
            &mut len,
        )
        .context("GetTokenInformation(TokenUser) failed")?;
        let user = &*buffer.as_ptr().cast::<TOKEN_USER>();

        let mut text = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut text)
            .context("ConvertSidToStringSidW failed")?;
        let sid = text.to_string();
        LocalFree(Some(HLOCAL(text.0.cast())));
        Ok(sid?)
    }
}

/// A security descriptor that grants full access to one user and nobody else.
#[derive(Debug)]
struct UserOnly(PSECURITY_DESCRIPTOR);

impl UserOnly {
    fn new(sddl: &HSTRING) -> io::Result<Self> {
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // SAFETY: `sddl` is a valid wide string; `descriptor` is a valid out-pointer.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl,
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )
        }
        .map_err(io::Error::from)?;
        Ok(Self(descriptor))
    }

    fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0.0,
            bInheritHandle: false.into(),
        }
    }
}

impl Drop for UserOnly {
    fn drop(&mut self) {
        // SAFETY: the descriptor came from ConvertStringSecurityDescriptor..., which
        // allocates with LocalAlloc, and is freed exactly once, here.
        unsafe { LocalFree(Some(HLOCAL(self.0.0))) };
    }
}

/// The server side of the pipe, owned by the notch.
#[derive(Debug)]
pub struct Listener {
    name: HSTRING,
    /// Security descriptor source for each new instance: a protected DACL with one
    /// allow-all entry for the current user.
    sddl: HSTRING,
    /// The instance the next client will connect to.
    next: OwnedHandle,
}

impl Listener {
    /// Claims the pipe name.
    ///
    /// # Errors
    ///
    /// Fails when another process already owns the name, which means another notch
    /// is running, or when the pipe cannot be created.
    pub fn bind(name: &str) -> anyhow::Result<Self> {
        let name = HSTRING::from(name);
        let sddl = HSTRING::from(format!("D:P(A;;GA;;;{})", current_user_sid()?));
        let next = match create_instance(&name, &sddl, Instance::First) {
            Ok(handle) => handle,
            Err(err) if err.raw_os_error() == Some(ERROR_ACCESS_DENIED.0 as i32) => {
                bail!("another ai-notch is already running ({name} is taken)")
            }
            Err(err) => return Err(err).with_context(|| format!("creating pipe {name}")),
        };
        Ok(Self { name, sddl, next })
    }

    /// Waits for the next hook process to connect and returns its connection.
    ///
    /// # Errors
    ///
    /// Fails when a client vanished before the connection completed (the instance is
    /// replaced, so the next call works) or when a new instance cannot be created.
    pub fn accept(&mut self) -> anyhow::Result<File> {
        let handle = HANDLE(self.next.as_raw_handle());
        // SAFETY: `handle` is a live pipe instance owned by `self.next`.
        let connected = match unsafe { ConnectNamedPipe(handle, None) } {
            Ok(()) => Ok(()),
            // A client connected between creation and this call; that is a success.
            Err(err) if err.code() == ERROR_PIPE_CONNECTED.to_hresult() => Ok(()),
            Err(err) => Err(err),
        };
        let fresh = create_instance(&self.name, &self.sddl, Instance::Additional)
            .with_context(|| format!("creating another instance of {}", self.name))?;
        let instance = mem::replace(&mut self.next, fresh);
        connected.context("ConnectNamedPipe failed")?;
        Ok(File::from(instance))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Instance {
    /// Fails if any process already created the name, so nobody can squat it.
    First,
    Additional,
}

fn create_instance(name: &HSTRING, sddl: &HSTRING, kind: Instance) -> io::Result<OwnedHandle> {
    let mut open_mode = PIPE_ACCESS_DUPLEX;
    if kind == Instance::First {
        open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
    }
    let security = UserOnly::new(sddl)?;
    let attributes = security.attributes();
    // SAFETY: `name` is a valid wide string and `attributes` points at a live
    // descriptor for the duration of the call.
    let handle = unsafe {
        CreateNamedPipeW(
            name,
            open_mode,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            BUFFER_BYTES,
            BUFFER_BYTES,
            0,
            Some(&attributes),
        )
    };
    if handle.is_invalid() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `handle` is a fresh, valid handle that nothing else owns.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle.0 as RawHandle) })
}

/// Connects to the notch's pipe.
///
/// # Errors
///
/// Fails with [`io::ErrorKind::NotFound`] when no notch is running, and with
/// [`io::ErrorKind::TimedOut`] when every instance stays busy.
pub fn connect(name: &str) -> io::Result<File> {
    for _ in 0..CONNECT_ATTEMPTS {
        // std opens with SECURITY_SQOS_PRESENT | SECURITY_ANONYMOUS, so the server
        // cannot impersonate the hook process.
        match OpenOptions::new().read(true).write(true).open(name) {
            Err(err) if err.raw_os_error() == Some(ERROR_PIPE_BUSY.0 as i32) => {
                // SAFETY: the name is a valid wide string for the duration of the call.
                let _ = unsafe { WaitNamedPipeW(&HSTRING::from(name), BUSY_WAIT_MS) };
            }
            other => return other,
        }
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "notch pipe stayed busy",
    ))
}

/// Whether the other end of a pipe connection has gone away.
#[must_use]
pub fn peer_closed(connection: &File) -> bool {
    let handle = HANDLE(connection.as_raw_handle().cast::<c_void>());
    // SAFETY: `handle` is a live pipe handle borrowed from `connection`; a zero-size
    // peek reads nothing and only reports the pipe state.
    unsafe { PeekNamedPipe(handle, None, 0, None, None, None) }.is_err()
}

#[cfg(test)]
mod tests {
    use std::io::BufReader;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::protocol::{self, Decision, Event, Origin};

    fn unique_name() -> String {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        format!(r"\\.\pipe\ai-notch-test-{}-{n}", std::process::id())
    }

    #[test]
    fn sid_looks_like_a_user_sid() {
        let sid = current_user_sid().expect("the test process has a user SID");
        assert!(sid.starts_with("S-1-5-"), "{sid}");
        assert!(pipe_name().expect("pipe name").ends_with(&sid));
    }

    #[test]
    fn request_and_reply_round_trip() {
        let name = unique_name();
        let mut listener = Listener::bind(&name).expect("fresh pipe name");
        let server = thread::spawn(move || {
            let connection = listener.accept().expect("client connects");
            let mut reader = BufReader::new(&connection);
            let event: Event = protocol::read_line(&mut reader)
                .expect("valid line")
                .expect("client sent an event");
            protocol::write_line(&mut &connection, &Decision::Allow).expect("reply");
            event
        });

        let client = connect(&name).expect("server is listening");
        let sent = Event::TaskDone {
            origin: Origin {
                session_id: "s".to_owned(),
                project: "p".to_owned(),
                ancestors: vec![1],
            },
            message: String::new(),
        };
        protocol::write_line(&mut &client, &sent).expect("send");
        let reply: Decision = protocol::read_line(&mut BufReader::new(&client))
            .expect("valid reply")
            .expect("server replied");

        assert_eq!(reply, Decision::Allow);
        assert_eq!(server.join().expect("server thread"), sent);
    }

    #[test]
    fn second_listener_on_the_same_name_is_refused() {
        let name = unique_name();
        let _first = Listener::bind(&name).expect("fresh pipe name");
        let second = Listener::bind(&name).expect_err("name is taken");
        assert!(second.to_string().contains("already running"), "{second:#}");
    }

    #[test]
    fn connect_without_a_notch_is_not_found() {
        let err = connect(&unique_name()).expect_err("nobody listens");
        assert_eq!(err.kind(), io::ErrorKind::NotFound, "{err}");
    }

    #[test]
    fn server_notices_when_the_client_leaves() {
        let name = unique_name();
        let mut listener = Listener::bind(&name).expect("fresh pipe name");
        let server = thread::spawn(move || listener.accept().expect("client connects"));
        let client = connect(&name).expect("server is listening");
        let connection = server.join().expect("server thread");

        assert!(!peer_closed(&connection));
        drop(client);
        let deadline = Instant::now() + Duration::from_secs(2);
        while !peer_closed(&connection) {
            assert!(Instant::now() < deadline, "disconnect never noticed");
            thread::sleep(Duration::from_millis(10));
        }
    }
}
