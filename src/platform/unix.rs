use std::fs::File;
use std::io::{self, Read, Write};
use std::num::NonZeroU32;
use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

use crate::model::ids::ProcessIdentity;

/// Create a pipe. Returns (read_fd, write_fd) as owned Files.
/// Both fds have close-on-exec set by default.
pub fn pipe() -> io::Result<(File, File)> {
    let mut fds = [0i32; 2];
    let ret = unsafe { libc::pipe(fds.as_mut_ptr()) };
    if ret != 0 {
        return Err(io::Error::last_os_error());
    }
    // Set close-on-exec on both ends
    for &fd in &fds {
        unsafe {
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        }
    }
    let read = unsafe { File::from_raw_fd(fds[0]) };
    let write = unsafe { File::from_raw_fd(fds[1]) };
    Ok((read, write))
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

    // Safety: pre_exec runs after fork, before exec in the child.
    // We clear close-on-exec on the ready fd so it survives exec,
    // and call setsid to detach from the parent's session.
    unsafe {
        cmd.pre_exec(move || {
            // Clear close-on-exec so the ready fd survives exec
            libc::fcntl(write_fd_raw, libc::F_SETFD, 0);
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

/// Write a readiness signal to the pipe.
pub fn write_ready_signal(fd_num: RawFd, message: &str) -> io::Result<()> {
    let mut file = unsafe { File::from_raw_fd(fd_num) };
    file.write_all(message.as_bytes())?;
    // file is dropped here, closing the fd
    Ok(())
}

/// Get the ProcessIdentity of the current process.
pub fn self_identity() -> io::Result<ProcessIdentity> {
    let pid = std::process::id();
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
    let ticks_per_sec = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as u64;
    Ok(ticks * (1_000_000_000 / ticks_per_sec))
}

#[cfg(target_os = "macos")]
fn process_start_time(pid: u32) -> io::Result<u64> {
    use std::mem;

    let mut info: libc::proc_bsdinfo = unsafe { mem::zeroed() };
    let size = mem::size_of::<libc::proc_bsdinfo>() as i32;

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
