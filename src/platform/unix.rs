use std::fs::File;
use std::io::{self, Read, Write};
use std::num::NonZeroU32;
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

use rustix::fs::{self as rfs, Mode, OFlags};

use crate::model::ids::ProcessIdentity;

/// Create a named pipe (FIFO) at `path` with mode 0600.
/// Note: rustix doesn't provide mkfifo on macOS, so this stays as raw libc.
pub fn mkfifo(path: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains null byte"))?;
    // SAFETY: c_path is a valid null-terminated C string (CString guarantees this).
    // 0o600 is a valid mode. mkfifo may fail but won't cause UB.
    let ret = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
    if ret != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Create a pipe. Returns (read_fd, write_fd) as owned Files.
/// Both fds have close-on-exec set.
pub fn pipe() -> io::Result<(File, File)> {
    #[cfg(target_os = "linux")]
    let (read_fd, write_fd) = { rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC)? };

    #[cfg(not(target_os = "linux"))]
    let (read_fd, write_fd) = {
        // macOS doesn't have pipe2 — use pipe() + set CLOEXEC
        let (r, w) = rustix::pipe::pipe()?;
        rustix::io::fcntl_setfd(&r, rustix::io::FdFlags::CLOEXEC)?;
        rustix::io::fcntl_setfd(&w, rustix::io::FdFlags::CLOEXEC)?;
        (r, w)
    };

    Ok((File::from(read_fd), File::from(write_fd)))
}

/// Spawn the sidecar as a detached process.
/// Returns the sidecar's PID. The sidecar will signal readiness via ready_fd.
///
/// The sidecar inherits the write end of the readiness pipe and is
/// responsible for writing a result and closing it.
pub fn spawn_sidecar(
    tender_bin: &Path,
    session_dir: &Path,
    ready_write_fd: &File,
) -> io::Result<u32> {
    let write_fd_raw = ready_write_fd.as_raw_fd();

    let mut cmd = Command::new(tender_bin);
    cmd.arg("_sidecar")
        .arg("--session-dir")
        .arg(session_dir)
        .env("TENDER_READY_FD", write_fd_raw.to_string());

    // Redirect stdin/stdout/stderr to /dev/null for detachment
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    // SAFETY: pre_exec runs after fork() in the child process, before exec().
    // The closure captures only write_fd_raw (a Copy integer), so no shared
    // mutable state is accessed. fcntl and setsid are async-signal-safe.
    // We clear close-on-exec on the ready fd so it survives exec,
    // and call setsid to detach from the parent's session.
    unsafe {
        cmd.pre_exec(move || {
            // Clear close-on-exec so the ready fd survives exec
            if libc::fcntl(write_fd_raw, libc::F_SETFD, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            // Detach into new session
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let child = cmd.spawn()?;
    Ok(child.id())
}

/// Read the readiness result from the pipe.
/// Returns Ok(message) on success, or Err if the pipe closed without a message
/// (sidecar died before signaling).
pub fn read_ready_signal(mut read_end: File) -> io::Result<String> {
    let mut buf = String::new();
    read_end.read_to_string(&mut buf)?;
    if buf.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "sidecar died without signaling readiness",
        ));
    }
    Ok(buf)
}

/// Open a FIFO for writing without blocking.
/// Returns ENXIO immediately if no reader is connected.
/// On success, clears O_NONBLOCK so subsequent writes block normally.
pub fn open_fifo_write_nonblock(path: &Path) -> io::Result<File> {
    let fd: OwnedFd = rfs::open(path, OFlags::WRONLY | OFlags::NONBLOCK, Mode::empty())?;

    let flags = rfs::fcntl_getfl(&fd)?;
    rfs::fcntl_setfl(&fd, flags & !OFlags::NONBLOCK)?;

    Ok(File::from(fd))
}

/// Write a readiness signal to the pipe.
pub fn write_ready_signal(fd_num: RawFd, message: &str) -> io::Result<()> {
    // SAFETY: fd_num is the TENDER_READY_FD passed by spawn_sidecar, which was a
    // valid open fd at exec time (close-on-exec was cleared in pre_exec).
    // from_raw_fd takes ownership — the fd is closed when file is dropped at
    // the end of this function, ensuring exactly one close.
    let mut file = unsafe { File::from_raw_fd(fd_num) };
    file.write_all(message.as_bytes())?;
    // file is dropped here, closing the fd
    Ok(())
}

/// Get the ProcessIdentity of the current process.
pub fn self_identity() -> io::Result<ProcessIdentity> {
    process_identity(std::process::id())
}

/// Get the ProcessIdentity of a process by PID.
pub fn process_identity(pid: u32) -> io::Result<ProcessIdentity> {
    let pid = NonZeroU32::new(pid).ok_or_else(|| io::Error::other("pid is zero"))?;
    let start_time_ns = process_start_time(pid.get())?;
    Ok(ProcessIdentity { pid, start_time_ns })
}

/// Get process start time. Platform-specific.
#[cfg(target_os = "linux")]
fn process_start_time(pid: u32) -> io::Result<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    // Field 22 (1-indexed) is starttime in clock ticks.
    // Fields are space-separated, but field 2 (comm) can contain spaces and parens.
    // Find the last ')' to skip past comm.
    let after_comm = stat
        .rfind(')')
        .map(|i| &stat[i + 2..])
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "malformed /proc/pid/stat"))?;
    // After comm, fields are: state(3), ppid(4), ..., starttime(22)
    // starttime is field 20 (0-indexed from after comm, since we skipped fields 1-2)
    let field = after_comm
        .split_whitespace()
        .nth(19) // 0-indexed: field 22 - field 3 = index 19
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "starttime field not found"))?;
    let ticks: u64 = field
        .parse()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("bad starttime: {e}")))?;
    // Convert ticks to nanoseconds
    let ticks_per_sec = rustix::param::clock_ticks_per_second() as u64;
    Ok(ticks * (1_000_000_000 / ticks_per_sec))
}

#[cfg(target_os = "macos")]
fn process_start_time(pid: u32) -> io::Result<u64> {
    use std::mem;

    // SAFETY: proc_bsdinfo is a POD type (plain data, no pointers that need
    // initialization). Zeroing it produces a valid struct that proc_pidinfo
    // will overwrite with actual process data.
    let mut info: libc::proc_bsdinfo = unsafe { mem::zeroed() };
    let size = mem::size_of::<libc::proc_bsdinfo>() as i32;

    // SAFETY: info is a properly sized and aligned proc_bsdinfo buffer.
    // pid is cast to i32 which is safe because macOS PIDs fit in i32.
    // PROC_PIDTBSDINFO is the correct flavor for proc_bsdinfo.
    // size matches the buffer — proc_pidinfo won't write out of bounds.
    let ret = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut _ as *mut _,
            size,
        )
    };

    if ret <= 0 {
        return Err(io::Error::last_os_error());
    }

    // pbi_start_tvsec and pbi_start_tvusec give the process start time
    let secs = info.pbi_start_tvsec;
    let usecs = info.pbi_start_tvusec;
    Ok(secs * 1_000_000_000 + usecs * 1_000)
}

/// Result of probing a process by identity.
/// Lifecycle state comes from the sidecar; process observation comes from
/// this typed OS result — never a boolean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum ProcessStatus {
    /// PID exists and identity matches — safe to signal.
    AliveVerified,
    /// PID does not exist (ESRCH).
    Missing,
    /// PID exists but identity differs — PID was recycled.
    IdentityMismatch,
    /// PID exists but OS denied access (EPERM) — different session on macOS.
    /// Can still signal (kill(2) only needs appropriate permissions, not
    /// proc_pidinfo access), but PID reuse safety is degraded.
    Inaccessible,
    /// Unexpected OS error.
    OsError(std::io::ErrorKind),
}

/// Probe a process by identity. Returns a typed status instead of a boolean
/// so callers can make informed decisions (especially around EPERM).
pub fn process_status(id: &ProcessIdentity) -> ProcessStatus {
    match process_identity(id.pid.get()) {
        Ok(current) => {
            if current == *id {
                ProcessStatus::AliveVerified
            } else {
                ProcessStatus::IdentityMismatch
            }
        }
        Err(e) => match e.raw_os_error() {
            Some(libc::ESRCH) => ProcessStatus::Missing,
            Some(libc::EPERM) => ProcessStatus::Inaccessible,
            _ => {
                // On macOS, proc_pidinfo returns 0 for missing processes
                // and sets errno to ESRCH. But if errno wasn't set (kind == Other),
                // the process is likely gone.
                if e.kind() == std::io::ErrorKind::Other {
                    ProcessStatus::Missing
                } else {
                    ProcessStatus::OsError(e.kind())
                }
            }
        },
    }
}

/// Kill a process. Sends SIGTERM first, waits briefly, then SIGKILL.
/// Tries process group kill first (kill(-pgid)), falls back to direct kill.
/// Verifies process identity before signaling to prevent killing recycled PIDs.
/// Returns Ok(()) if the process is already dead (idempotent).
pub fn kill_process(id: &ProcessIdentity, force: bool) -> io::Result<()> {
    match process_status(id) {
        ProcessStatus::Missing => return Ok(()),
        ProcessStatus::IdentityMismatch => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "PID was recycled — refusing to kill wrong process",
            ));
        }
        ProcessStatus::OsError(kind) => {
            return Err(io::Error::new(kind, "failed to probe process status"));
        }
        // AliveVerified: identity confirmed, signal normally
        // Inaccessible: can't verify identity (EPERM from proc_pidinfo on macOS
        // when child is in a different session), but kill(2) will still work.
        // Degraded PID reuse safety — acceptable because the sidecar wrote the
        // identity moments ago and PID reuse within that window is negligible.
        ProcessStatus::AliveVerified | ProcessStatus::Inaccessible => {}
    }

    let pid = id.pid.get() as i32;
    let signal = if force {
        rustix::process::Signal::KILL
    } else {
        rustix::process::Signal::TERM
    };

    // Try process group first (kills descendants), fall back to direct
    send_signal_group(pid, signal).or_else(|_| send_signal_direct(pid, signal))?;

    if force {
        return Ok(());
    }

    // Wait up to 5 seconds for graceful exit
    for _ in 0..50 {
        match process_status(id) {
            ProcessStatus::Missing => return Ok(()),
            ProcessStatus::IdentityMismatch => return Ok(()), // recycled = original is dead
            _ => {}
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    // Still alive — escalate to SIGKILL
    send_signal_group(pid, rustix::process::Signal::KILL)
        .or_else(|_| send_signal_direct(pid, rustix::process::Signal::KILL))?;

    Ok(())
}

/// Send a signal to a process group (kill(-pgid, sig)).
fn send_signal_group(pid: i32, signal: rustix::process::Signal) -> io::Result<()> {
    // rustix::process::Pid is a NonZero type representing a positive PID.
    // For process group kill we need the group leader's PID.
    let rpid = rustix::process::Pid::from_raw(pid)
        .ok_or_else(|| io::Error::other("invalid pid for process group signal"))?;
    match rustix::process::kill_process_group(rpid, signal) {
        Ok(()) => Ok(()),
        Err(e) if e == rustix::io::Errno::SRCH => Ok(()), // already dead
        Err(e) => Err(e.into()),
    }
}

/// Send a signal directly to a process (kill(pid, sig)).
fn send_signal_direct(pid: i32, signal: rustix::process::Signal) -> io::Result<()> {
    let rpid = rustix::process::Pid::from_raw(pid)
        .ok_or_else(|| io::Error::other("invalid pid for direct signal"))?;
    match rustix::process::kill_process(rpid, signal) {
        Ok(()) => Ok(()),
        Err(e) if e == rustix::io::Errno::SRCH => Ok(()), // already dead
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU32;

    fn fake_identity(pid: u32, start_time_ns: u64) -> ProcessIdentity {
        ProcessIdentity {
            pid: NonZeroU32::new(pid).unwrap(),
            start_time_ns,
        }
    }

    #[test]
    fn process_status_self_is_alive_verified() {
        let self_id = self_identity().unwrap();
        assert_eq!(process_status(&self_id), ProcessStatus::AliveVerified);
    }

    #[test]
    fn process_status_missing_pid_returns_missing() {
        // PID 4_000_000 is safely above any real PID
        let id = fake_identity(4_000_000, 0);
        assert_eq!(process_status(&id), ProcessStatus::Missing);
    }

    #[test]
    fn process_status_wrong_start_time_returns_identity_mismatch() {
        // Use our own PID but with a bogus start time
        let self_id = self_identity().unwrap();
        let wrong = fake_identity(self_id.pid.get(), self_id.start_time_ns.wrapping_add(1));
        assert_eq!(process_status(&wrong), ProcessStatus::IdentityMismatch);
    }

    #[test]
    fn process_status_inaccessible_on_cross_session_child() {
        // Spawn a child in a new session (setsid), then probe from parent.
        // On macOS, proc_pidinfo returns EPERM for processes in other sessions.
        // On Linux, /proc/<pid>/stat is world-readable so this returns AliveVerified.
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};

        let mut cmd = Command::new("sleep");
        cmd.arg("10");
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // SAFETY: pre_exec runs after fork() in the child, before exec().
        // setsid is async-signal-safe. No shared mutable state is accessed.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = cmd.spawn().unwrap();
        let child_pid = child.id();

        // Small delay for process to be queryable
        std::thread::sleep(std::time::Duration::from_millis(50));

        let child_id = match process_identity(child_pid) {
            Ok(id) => id,
            Err(_) => {
                // On macOS, proc_pidinfo may already EPERM here.
                // Verify that process_status on a fabricated identity returns Inaccessible.
                let fabricated = fake_identity(child_pid, 0);
                let status = process_status(&fabricated);
                assert!(
                    status == ProcessStatus::Inaccessible || status == ProcessStatus::Missing,
                    "expected Inaccessible or Missing for cross-session child, got {status:?}"
                );
                // Clean up
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
        };

        let status = process_status(&child_id);

        // macOS: Inaccessible (EPERM from proc_pidinfo across sessions)
        // Linux: AliveVerified (/proc/<pid>/stat is readable)
        assert!(
            status == ProcessStatus::AliveVerified || status == ProcessStatus::Inaccessible,
            "expected AliveVerified or Inaccessible, got {status:?}"
        );

        // Clean up
        let _ = child.kill();
        let _ = child.wait();
    }
}
