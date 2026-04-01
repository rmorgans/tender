use std::fs::File;

use tender::model::ids::{Namespace, SessionName};
use tender::model::spec::StdinMode;
use tender::model::state::RunStatus;
use tender::session::{self, SessionDir, SessionRoot};

/// Advisory flock on `session_dir/exec.lock`, non-blocking.
/// Ensures only one exec runs on a session at a time.
#[derive(Debug)]
pub struct ExecLock {
    _file: File,
}

#[cfg(unix)]
impl ExecLock {
    /// Try to acquire exec lock. Fails immediately if another exec holds it.
    pub fn try_acquire(session: &SessionDir) -> anyhow::Result<Self> {
        use std::os::unix::io::AsRawFd;

        let lock_path = session.path().join("exec.lock");
        let file = File::create(&lock_path)?;

        // SAFETY: file is an open File, so as_raw_fd() returns a valid fd.
        // LOCK_EX | LOCK_NB is a valid flock operation (non-blocking exclusive).
        // flock may fail (EWOULDBLOCK) but won't cause UB.
        let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if ret != 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::WouldBlock {
                anyhow::bail!("another exec is already running on this session");
            }
            return Err(err.into());
        }

        Ok(Self { _file: file })
    }
}

#[cfg(windows)]
impl ExecLock {
    /// Try to acquire exec lock. Fails immediately if another exec holds it.
    pub fn try_acquire(session: &SessionDir) -> anyhow::Result<Self> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::HANDLE;
        use windows_sys::Win32::Storage::FileSystem::{
            LockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
        };

        let lock_path = session.path().join("exec.lock");
        let file = File::create(&lock_path)?;

        let mut overlapped: windows_sys::Win32::System::IO::OVERLAPPED = unsafe { std::mem::zeroed() };
        let ret = unsafe {
            LockFileEx(
                file.as_raw_handle() as HANDLE,
                LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                0,
                1,
                0,
                &mut overlapped,
            )
        };
        if ret == 0 {
            let err = std::io::Error::last_os_error();
            anyhow::bail!("another exec is already running on this session: {err}");
        }

        Ok(Self { _file: file })
    }
}

pub fn cmd_exec(
    name: &str,
    cmd: Vec<String>,
    _timeout: Option<u64>,
    namespace: &Namespace,
) -> anyhow::Result<()> {
    let session_name = SessionName::new(name)?;
    let root = SessionRoot::default_path()?;

    let session = session::open(&root, namespace, &session_name)?
        .ok_or_else(|| anyhow::anyhow!("session not found: {name}"))?;

    let meta = session::read_meta(&session)?;

    if !matches!(meta.status(), RunStatus::Running { .. }) {
        anyhow::bail!("session is not running");
    }

    if meta.launch_spec().stdin_mode != StdinMode::Pipe {
        anyhow::bail!("session was not started with --stdin");
    }

    let _lock = ExecLock::try_acquire(&session)?;

    // Validate cmd is non-empty (clap enforces this, but belt-and-suspenders)
    if cmd.is_empty() {
        anyhow::bail!("no command specified");
    }

    anyhow::bail!("exec not yet implemented");
}
