//! Windows implementation of the Platform trait.
//!
//! 2A.2: Module skeleton with process identity/status implemented,
//! all other methods return Unsupported errors.

use std::collections::BTreeMap;
use std::fs::File;
use std::io;
use std::num::NonZeroU32;
use std::path::Path;
use std::process::ExitStatus;

use crate::model::ids::ProcessIdentity;
use crate::platform::{Platform, ProcessStatus};

/// Windows implementation of the Platform trait.
pub struct WindowsPlatform;

/// Opaque supervised-child state for Windows.
/// Will hold: process HANDLE, Job Object HANDLE, stop event HANDLE, I/O pipes.
pub struct SupervisedChild {
    _private: (), // placeholder — real fields added in 2A.3
}

/// Lightweight kill handle for Windows.
/// Will hold: Job Object HANDLE + stop event HANDLE for tree kill.
#[derive(Clone)]
pub struct ChildKillHandle {
    #[allow(dead_code)] // used when kill_child is implemented in 2A.3
    identity: ProcessIdentity,
}

/// Stdin transport for Windows.
/// Will hold: server-side named pipe HANDLE.
pub struct StdinTransport {
    _private: (), // placeholder — real fields added in 2A.5
}

// SAFETY: StdinTransport will hold HANDLEs which are Send on Windows.
unsafe impl Send for StdinTransport {}

fn unsupported(what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        format!("windows: {what} not yet implemented"),
    )
}

impl Platform for WindowsPlatform {
    type SupervisedChild = SupervisedChild;
    type ChildKillHandle = ChildKillHandle;
    type StdinTransport = StdinTransport;
    type ReadyReader = File;
    type ReadyWriter = File;

    fn spawn_sidecar(
        _tender_bin: &Path,
        _session_dir: &Path,
        _ready_writer: &File,
    ) -> io::Result<u32> {
        Err(unsupported("spawn_sidecar"))
    }

    fn ready_channel() -> io::Result<(File, File)> {
        Err(unsupported("ready_channel"))
    }

    fn read_ready_signal(_reader: File) -> io::Result<String> {
        Err(unsupported("read_ready_signal"))
    }

    fn write_ready_signal(_writer: File, _message: &str) -> io::Result<()> {
        Err(unsupported("write_ready_signal"))
    }

    fn spawn_child(
        _argv: &[String],
        _stdin_piped: bool,
        _cwd: Option<&Path>,
        _env: &BTreeMap<String, String>,
    ) -> io::Result<SupervisedChild> {
        Err(unsupported("spawn_child"))
    }

    fn child_identity(_child: &SupervisedChild) -> io::Result<ProcessIdentity> {
        Err(unsupported("child_identity"))
    }

    fn child_wait(_child: &mut SupervisedChild) -> io::Result<ExitStatus> {
        Err(unsupported("child_wait"))
    }

    fn child_try_wait(_child: &mut SupervisedChild) -> io::Result<Option<ExitStatus>> {
        Err(unsupported("child_try_wait"))
    }

    fn child_stdout(_child: &mut SupervisedChild) -> Option<Box<dyn io::Read + Send>> {
        None
    }

    fn child_stderr(_child: &mut SupervisedChild) -> Option<Box<dyn io::Read + Send>> {
        None
    }

    fn child_stdin(_child: &mut SupervisedChild) -> Option<Box<dyn io::Write + Send>> {
        None
    }

    fn child_kill_handle(_child: &SupervisedChild) -> ChildKillHandle {
        // This can only be called after spawn_child succeeds, which
        // currently returns Err, so this is unreachable.
        unreachable!("child_kill_handle called without a spawned child")
    }

    fn kill_child(_handle: &ChildKillHandle, _force: bool) -> io::Result<()> {
        Err(unsupported("kill_child"))
    }

    fn kill_orphan(id: &ProcessIdentity, _force: bool) -> io::Result<()> {
        // Orphan kill with force=false degrades to force on Windows
        // (no stop event available for orphans). For now, return Unsupported.
        let _ = id;
        Err(unsupported("kill_orphan"))
    }

    fn self_identity() -> io::Result<ProcessIdentity> {
        let pid = std::process::id();
        process_identity(pid)
    }

    fn process_identity(pid: u32) -> io::Result<ProcessIdentity> {
        process_identity(pid)
    }

    fn process_status(id: &ProcessIdentity) -> ProcessStatus {
        process_status(id)
    }

    fn create_stdin_transport(_session_dir: &Path) -> io::Result<StdinTransport> {
        Err(unsupported("create_stdin_transport"))
    }

    fn accept_stdin_connection(
        _transport: &StdinTransport,
        _session_dir: &Path,
    ) -> Option<Box<dyn io::Read + Send>> {
        None
    }

    fn open_stdin_writer(_session_dir: &Path) -> io::Result<File> {
        Err(unsupported("open_stdin_writer"))
    }

    fn remove_stdin_transport(_session_dir: &Path) {
        // no-op until transport is implemented
    }

    fn ready_writer_from_env() -> io::Result<File> {
        Err(unsupported("ready_writer_from_env"))
    }

    fn seal_ready_fd(_writer: &File) -> io::Result<()> {
        // Windows uses precise HANDLE_LIST — no sealing needed.
        Ok(())
    }
}

// --- Process identity implementation ---

/// Get ProcessIdentity for a process by PID.
///
/// Uses `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` + `GetProcessTimes`.
/// `PROCESS_QUERY_LIMITED_INFORMATION` (0x1000) is the minimum access right
/// needed for `GetProcessTimes` — it works even for processes running as
/// other users, unlike `PROCESS_QUERY_INFORMATION` which requires elevated
/// privileges. This matches the Unix approach where `/proc/pid/stat` is
/// world-readable.
///
/// Error mapping:
/// - `ERROR_INVALID_PARAMETER` (87): PID does not exist
/// - `ERROR_ACCESS_DENIED` (5): process exists but we can't query it
///   (e.g., protected process, system process)
fn process_identity(pid: u32) -> io::Result<ProcessIdentity> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetCurrentProcessId, GetProcessTimes, OpenProcess,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };

    let pid_nz = NonZeroU32::new(pid).ok_or_else(|| io::Error::other("pid is zero"))?;

    let is_self = pid == unsafe { GetCurrentProcessId() };
    let handle = if is_self {
        // GetCurrentProcess returns a pseudo-handle (-1) that is always valid
        // for the current process and does not need CloseHandle.
        unsafe { GetCurrentProcess() }
    } else {
        // PROCESS_QUERY_LIMITED_INFORMATION is the minimum right for GetProcessTimes.
        // bInheritHandle = FALSE (0) — we don't need child inheritance.
        let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if h.is_null() {
            return Err(io::Error::last_os_error());
        }
        h
    };

    let mut creation = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut exit = creation;
    let mut kernel = creation;
    let mut user = creation;

    let ret = unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) };

    // Close handle unless it's the pseudo-handle for current process
    if !is_self {
        unsafe { CloseHandle(handle) };
    }

    if ret == 0 {
        return Err(io::Error::last_os_error());
    }

    // Convert FILETIME (100ns intervals since 1601-01-01) to nanoseconds since Unix epoch.
    // Epoch offset: 11,644,473,600 seconds = 116,444,736,000,000,000 100ns intervals.
    let ticks_100ns = (creation.dwHighDateTime as u64) << 32 | creation.dwLowDateTime as u64;
    let unix_ticks = ticks_100ns.saturating_sub(116_444_736_000_000_000);
    let start_time_ns = unix_ticks * 100; // 100ns → ns

    Ok(ProcessIdentity {
        pid: pid_nz,
        start_time_ns,
    })
}

/// Probe a process by identity on Windows.
///
/// Error classification from `OpenProcess`:
///
/// | Win32 error | Code | Meaning | Maps to |
/// |---|---|---|---|
/// | `ERROR_INVALID_PARAMETER` | 87 | PID does not exist (never existed or fully reaped) | `Missing` |
/// | `ERROR_ACCESS_DENIED` | 5 | Process exists but we can't query (protected/system) | `Inaccessible` |
/// | Other | — | Unexpected failure | `OsError` |
///
/// Note: Unlike Unix where PIDs are recycled immediately after exit,
/// Windows keeps the process object alive as long as any handle is open.
/// So `ERROR_INVALID_PARAMETER` reliably means "no process with this PID"
/// rather than "PID was recycled." The identity check (creation time)
/// catches the recycled-PID case when `OpenProcess` succeeds.
fn process_status(id: &ProcessIdentity) -> ProcessStatus {
    use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER};

    match process_identity(id.pid.get()) {
        Ok(current) => {
            if current == *id {
                ProcessStatus::AliveVerified
            } else {
                ProcessStatus::IdentityMismatch
            }
        }
        Err(e) => match e.raw_os_error() {
            Some(code) if code == ERROR_INVALID_PARAMETER as i32 => ProcessStatus::Missing,
            Some(code) if code == ERROR_ACCESS_DENIED as i32 => ProcessStatus::Inaccessible,
            _ => ProcessStatus::OsError(e.kind()),
        },
    }
}
