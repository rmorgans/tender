//! Windows implementation of the Platform trait.
//!
//! 2A.2: Module skeleton with process identity/status implemented,
//! all other methods return Unsupported errors.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Write};
use std::num::NonZeroU32;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Command, ExitStatus};
use std::sync::Arc;

use crate::model::ids::ProcessIdentity;
use crate::platform::{Platform, ProcessStatus};

/// Windows implementation of the Platform trait.
pub struct WindowsPlatform;

/// Opaque supervised-child state for Windows.
/// Wraps a `std::process::Child` with its verified `ProcessIdentity`
/// and an Arc'd Job Object for tree kill.
pub struct SupervisedChild {
    child: std::process::Child,
    identity: ProcessIdentity,
    job_object: Arc<OwnedHandle>,
}

/// Lightweight kill handle for Windows.
/// Carries an Arc'd Job Object HANDLE for tree kill and ProcessIdentity
/// for status checks and GenerateConsoleCtrlEvent targeting.
#[derive(Clone)]
pub struct ChildKillHandle {
    identity: ProcessIdentity,
    job_object: Arc<OwnedHandle>,
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
        tender_bin: &Path,
        session_dir: &Path,
        ready_writer: &File,
    ) -> io::Result<u32> {
        use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS};

        let handle_value = ready_writer.as_raw_handle() as usize;

        let mut cmd = Command::new(tender_bin);
        cmd.arg("_sidecar")
            .arg("--session-dir")
            .arg(session_dir)
            .env("TENDER_READY_HANDLE", handle_value.to_string());

        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());

        // DETACHED_PROCESS: sidecar has no console initially. It calls
        // prepare_sidecar_console() in its own startup to AllocConsole
        // + hide window before spawning children. See plan Decision #3.
        //
        // CREATE_NEW_PROCESS_GROUP: own process group, detached from
        // parent's signal routing.
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);

        let child = cmd.spawn()?;
        Ok(child.id())
    }

    fn ready_channel() -> io::Result<(File, File)> {
        use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
        use windows_sys::Win32::System::Pipes::CreatePipe;

        let mut sa: SECURITY_ATTRIBUTES = unsafe { std::mem::zeroed() };
        sa.nLength = std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32;
        sa.bInheritHandle = 1; // TRUE — both handles inheritable initially

        let mut read_handle = std::ptr::null_mut();
        let mut write_handle = std::ptr::null_mut();

        // SAFETY: sa is valid, pointers are valid out params.
        let ret = unsafe { CreatePipe(&mut read_handle, &mut write_handle, &sa, 0) };
        if ret == 0 {
            return Err(io::Error::last_os_error());
        }

        // Make read end non-inheritable — only the parent reads.
        set_handle_inheritable(read_handle, false)?;

        // SAFETY: both handles are valid from CreatePipe.
        let read_file = unsafe { File::from_raw_handle(read_handle as *mut _) };
        let write_file = unsafe { File::from_raw_handle(write_handle as *mut _) };

        Ok((read_file, write_file))
    }

    fn read_ready_signal(mut reader: File) -> io::Result<String> {
        let mut buf = String::new();
        reader.read_to_string(&mut buf)?;
        if buf.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "sidecar died without signaling readiness",
            ));
        }
        Ok(buf)
    }

    fn write_ready_signal(mut writer: File, message: &str) -> io::Result<()> {
        writer.write_all(message.as_bytes())?;
        // writer dropped here — closes HANDLE, reader sees EOF
        Ok(())
    }

    fn spawn_child(
        argv: &[String],
        stdin_piped: bool,
        cwd: Option<&Path>,
        env: &BTreeMap<String, String>,
    ) -> io::Result<SupervisedChild> {
        use windows_sys::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP;

        let mut cmd = Command::new(&argv[0]);
        if argv.len() > 1 {
            cmd.args(&argv[1..]);
        }
        if stdin_piped {
            cmd.stdin(std::process::Stdio::piped());
        } else {
            cmd.stdin(std::process::Stdio::null());
        }
        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }
        if !env.is_empty() {
            cmd.envs(env);
        }

        // CREATE_NEW_PROCESS_GROUP: required for GenerateConsoleCtrlEvent targeting.
        // No CREATE_SUSPENDED — std::process::Child doesn't expose the thread
        // handle needed for ResumeThread. See Decision #2 for the race trade-off.
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
        let mut child = cmd.spawn()?;

        let job = create_job_object()?;

        // Assign child to Job Object immediately after spawn.
        // Race window: child may briefly run before assignment. See Decision #2.
        if let Err(e) = assign_process_to_job(
            job.as_raw_handle() as *mut _,
            child.as_raw_handle() as *mut _,
        ) {
            // Kill the child so it doesn't run unsupervised outside the job.
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }

        let pid = child.id();
        let identity = process_identity(pid)?;

        Ok(SupervisedChild {
            child,
            identity,
            job_object: Arc::new(job),
        })
    }

    fn child_identity(child: &SupervisedChild) -> io::Result<ProcessIdentity> {
        Ok(child.identity)
    }

    fn child_wait(child: &mut SupervisedChild) -> io::Result<ExitStatus> {
        child.child.wait()
    }

    fn child_try_wait(child: &mut SupervisedChild) -> io::Result<Option<ExitStatus>> {
        child.child.try_wait()
    }

    fn child_stdout(child: &mut SupervisedChild) -> Option<Box<dyn io::Read + Send>> {
        child
            .child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn io::Read + Send>)
    }

    fn child_stderr(child: &mut SupervisedChild) -> Option<Box<dyn io::Read + Send>> {
        child
            .child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn io::Read + Send>)
    }

    fn child_stdin(child: &mut SupervisedChild) -> Option<Box<dyn io::Write + Send>> {
        child
            .child
            .stdin
            .take()
            .map(|s| Box::new(s) as Box<dyn io::Write + Send>)
    }

    fn child_kill_handle(child: &SupervisedChild) -> ChildKillHandle {
        ChildKillHandle {
            identity: child.identity,
            job_object: child.job_object.clone(),
        }
    }

    fn kill_child(handle: &ChildKillHandle, force: bool) -> io::Result<()> {
        if force {
            return terminate_job(&handle.job_object);
        }

        // Graceful: best-effort CTRL_BREAK, then poll via WaitForSingleObject,
        // then escalate. Uses a real waitable handle instead of process_status
        // because Windows keeps the process object alive while any handle is
        // open (e.g. SupervisedChild's std::process::Child), making PID-based
        // liveness checks unreliable.
        send_ctrl_break(handle.identity.pid.get());

        if wait_for_process_exit(handle.identity.pid.get(), 5000) {
            return Ok(());
        }

        // Grace period expired — escalate to force.
        terminate_job(&handle.job_object)
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
        let handle_str = std::env::var("TENDER_READY_HANDLE")
            .map_err(|_| io::Error::new(io::ErrorKind::NotFound, "TENDER_READY_HANDLE not set"))?;
        let handle: usize = handle_str.parse().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "TENDER_READY_HANDLE is not a valid handle",
            )
        })?;
        // SAFETY: handle was inherited from the parent via CreatePipe with
        // bInheritHandle=TRUE. from_raw_handle takes ownership.
        Ok(unsafe { File::from_raw_handle(handle as *mut _) })
    }

    fn seal_ready_fd(writer: File) -> io::Result<File> {
        // Replace the inheritable HANDLE with a non-inheritable duplicate.
        // Takes ownership of the old File (closing the inheritable handle)
        // and returns a new File wrapping a non-inheritable copy.
        use windows_sys::Win32::Foundation::{DuplicateHandle, DUPLICATE_SAME_ACCESS};
        use windows_sys::Win32::System::Threading::GetCurrentProcess;

        let old_handle = writer.into_raw_handle() as *mut _;
        let current_process = unsafe { GetCurrentProcess() };
        let mut new_handle: *mut core::ffi::c_void = std::ptr::null_mut();

        let ret = unsafe {
            DuplicateHandle(
                current_process,
                old_handle,
                current_process,
                &mut new_handle,
                0,    // ignored with DUPLICATE_SAME_ACCESS
                0,    // bInheritHandle = FALSE
                DUPLICATE_SAME_ACCESS,
            )
        };

        // Close the original inheritable handle.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(old_handle) };

        if ret == 0 {
            return Err(io::Error::last_os_error());
        }

        // SAFETY: new_handle is a valid, non-inheritable duplicate.
        Ok(unsafe { File::from_raw_handle(new_handle) })
    }
}

// --- Sidecar runtime ---

/// Allocate a hidden console for the sidecar process.
///
/// The sidecar spawns with DETACHED_PROCESS (no console). Before spawning
/// children, it must acquire a console so that:
/// - Children inherit it via CREATE_NEW_PROCESS_GROUP
/// - GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid) can reach them
///
/// The ShowWindow(SW_HIDE) step is best-effort — AllocConsole is the
/// important part. If the window can't be hidden, graceful stop still works.
pub fn prepare_sidecar_console() {
    use windows_sys::Win32::System::Console::{AllocConsole, GetConsoleWindow};
    use windows_sys::Win32::UI::WindowsAndMessaging::{SW_HIDE, ShowWindow};

    // SAFETY: AllocConsole is safe to call. Returns FALSE if the process
    // already has a console — harmless.
    unsafe { AllocConsole() };

    // Best-effort: hide the console window.
    let hwnd = unsafe { GetConsoleWindow() };
    if !hwnd.is_null() {
        unsafe { ShowWindow(hwnd, SW_HIDE) };
    }
}

// --- Handle helpers ---

/// Set or clear the inheritable flag on a HANDLE.
fn set_handle_inheritable(
    handle: windows_sys::Win32::Foundation::HANDLE,
    inheritable: bool,
) -> io::Result<()> {
    use windows_sys::Win32::Foundation::HANDLE_FLAG_INHERIT;
    use windows_sys::Win32::Foundation::SetHandleInformation;

    let flags = if inheritable { HANDLE_FLAG_INHERIT } else { 0 };
    let ret = unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, flags) };
    if ret == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

// --- Job Object helpers ---

/// Create a Job Object with KILL_ON_JOB_CLOSE (safety net for crashes).
fn create_job_object() -> io::Result<OwnedHandle> {
    use windows_sys::Win32::System::JobObjects::{
        CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, SetInformationJobObject,
    };

    // SAFETY: null name = anonymous job object. Returns null on failure.
    let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }

    // Configure kill-on-close before returning, while we still have the raw handle.
    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

    let ret = unsafe {
        SetInformationJobObject(
            handle,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };
    if ret == 0 {
        // Close the job handle before returning the error.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
        return Err(io::Error::last_os_error());
    }

    // SAFETY: handle is a valid non-null HANDLE from CreateJobObjectW.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle as *mut _) })
}

/// Assign a process to a Job Object.
fn assign_process_to_job(
    job: windows_sys::Win32::Foundation::HANDLE,
    process: windows_sys::Win32::Foundation::HANDLE,
) -> io::Result<()> {
    use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;

    // SAFETY: both handles are valid — job from create_job_object,
    // process from std::process::Child (which owns the process HANDLE).
    let ret = unsafe { AssignProcessToJobObject(job, process) };
    if ret == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Terminate all processes in a Job Object. Idempotent — already-dead is Ok.
fn terminate_job(job: &OwnedHandle) -> io::Result<()> {
    use windows_sys::Win32::System::JobObjects::TerminateJobObject;

    let ret = unsafe { TerminateJobObject(job.as_raw_handle() as *mut _, 1) };
    if ret == 0 {
        let err = io::Error::last_os_error();
        // ERROR_ACCESS_DENIED can mean the job is already terminated.
        if err.raw_os_error() != Some(windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED as i32) {
            return Err(err);
        }
    }
    Ok(())
}

/// Wait for a process to exit using WaitForSingleObject.
/// Opens a SYNCHRONIZE handle to the process by PID, waits up to `timeout_ms`.
/// Returns true if the process exited within the timeout, false otherwise.
/// Returns true (treat as exited) if the process handle cannot be opened
/// (process already gone or access denied).
fn wait_for_process_exit(pid: u32, timeout_ms: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };

    // SAFETY: OpenProcess with SYNCHRONIZE is the minimum right for waiting.
    let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    if handle.is_null() {
        // Can't open — process likely already exited.
        return true;
    }

    // SAFETY: handle is valid from OpenProcess. WaitForSingleObject blocks
    // until the process exits or the timeout expires.
    let result = unsafe { WaitForSingleObject(handle, timeout_ms) };
    unsafe { CloseHandle(handle) };

    result == WAIT_OBJECT_0
}

/// Best-effort CTRL_BREAK to a process group. No-op if the child has no console.
fn send_ctrl_break(pid: u32) {
    use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent};
    // The child was created with CREATE_NEW_PROCESS_GROUP, so pid == group id.
    // Failure is silently ignored — this is a best-effort graceful stop.
    unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid) };
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
