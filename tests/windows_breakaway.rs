#![cfg(windows)]

//! Verify the sidecar survives termination of its parent's Job Object.
//!
//! This is the SSH-disconnect survival case: OpenSSH-Server on Windows wraps
//! each session's spawned processes in a Job Object with KILL_ON_JOB_CLOSE
//! so it can guarantee cleanup on disconnect. Without
//! CREATE_BREAKAWAY_FROM_JOB on the sidecar's CreateProcessW call, the
//! sidecar inherits that job — `DETACHED_PROCESS` only severs the console,
//! not the job — and dies on disconnect.
//!
//! The test reproduces the kill chain locally:
//!   1. Create a named Job Object with KILL_ON_JOB_CLOSE | BREAKAWAY_OK
//!   2. Spawn helper which assigns self to job and then spawns `tender start`
//!   3. After helper exits, all in-job processes have either inherited the
//!      job (`tender start` did, sidecar would without breakaway) or broken
//!      away (sidecar should, with the fix)
//!   4. TerminateJobObject — kills anything still in the job
//!   5. Assert sidecar PID still alive

use std::ptr;
use std::time::Duration;

use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::JobObjects::{
    CreateJobObjectW, JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};

const STILL_ACTIVE: u32 = 259;

fn create_named_job_with_limits(name: &str, limit_flags: u32) -> *mut std::ffi::c_void {
    let name_w: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: name pointer is valid for the duration of the call.
    let job = unsafe { CreateJobObjectW(ptr::null(), name_w.as_ptr()) };
    assert!(
        !job.is_null(),
        "CreateJobObjectW failed: {}",
        std::io::Error::last_os_error()
    );

    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    info.BasicLimitInformation.LimitFlags = limit_flags;
    let ret = unsafe {
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };
    assert!(
        ret != 0,
        "SetInformationJobObject failed: {}",
        std::io::Error::last_os_error()
    );

    job
}

fn process_alive(pid: u32) -> bool {
    // SAFETY: OpenProcess is safe; null check guards the rest.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return false;
        }
        let mut code: u32 = 0;
        let ok = GetExitCodeProcess(h, &mut code);
        CloseHandle(h);
        ok != 0 && code == STILL_ACTIVE
    }
}

/// Resolve a binary path: prefer runtime env var (so the test can be moved to
/// another machine alongside the binaries), fall back to compile-time
/// CARGO_BIN_EXE for local cargo-test runs.
fn resolve_bin(env_key: &str, compile_time: &str) -> String {
    std::env::var(env_key).unwrap_or_else(|_| compile_time.to_string())
}

#[test]
fn sidecar_survives_parent_job_kill() {
    let tender_bin = resolve_bin("TENDER_TEST_BIN", env!("CARGO_BIN_EXE_tender"));
    let helper_bin = resolve_bin(
        "TENDER_TEST_HELPER_BIN",
        env!("CARGO_BIN_EXE_test_breakaway_parent"),
    );
    let home = tempfile::tempdir().expect("tempdir");
    let session = format!("breakaway-{}", std::process::id());
    let sidecar_pid_out = home.path().join("sidecar_pid.txt");
    let job_name = format!("tender-test-breakaway-{}", std::process::id());

    let job = create_named_job_with_limits(
        &job_name,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_BREAKAWAY_OK,
    );

    let status = std::process::Command::new(&helper_bin)
        .arg(&tender_bin)
        .arg(home.path())
        .arg(&session)
        .arg(&sidecar_pid_out)
        .env("TEST_JOB_NAME", &job_name)
        .status()
        .expect("spawn helper");
    assert!(status.success(), "helper failed: {status:?}");

    let pid_str =
        std::fs::read_to_string(&sidecar_pid_out).expect("read sidecar_pid_out from helper");
    let sidecar_pid: u32 = pid_str.trim().parse().expect("parse sidecar pid");
    assert!(
        process_alive(sidecar_pid),
        "sidecar pid {sidecar_pid} should be alive immediately after helper exit"
    );

    // The kill: terminate the parent job. Without CREATE_BREAKAWAY_FROM_JOB,
    // the sidecar is in this job and dies here.
    let ret = unsafe { TerminateJobObject(job, 1) };
    assert!(
        ret != 0,
        "TerminateJobObject failed: {}",
        std::io::Error::last_os_error()
    );

    // Allow the kernel a moment to propagate the termination.
    std::thread::sleep(Duration::from_millis(500));

    let survived = process_alive(sidecar_pid);

    // Always attempt cleanup of the test session before asserting.
    let _ = std::process::Command::new(tender_bin)
        .env("HOME", home.path())
        .args(["kill", &session, "--force"])
        .status();
    unsafe { CloseHandle(job) };

    assert!(
        survived,
        "sidecar pid {sidecar_pid} was killed by parent job termination — \
         CREATE_BREAKAWAY_FROM_JOB likely missing from sidecar spawn flags"
    );
}

/// Fallback path: when the parent's job forbids breakaway, `tender start`
/// must still succeed. Without the fallback, CreateProcessW returns
/// ERROR_ACCESS_DENIED and `tender start` fails outright — strictly worse
/// than the degraded case where the sidecar inherits the parent's lifetime.
#[test]
fn sidecar_spawn_succeeds_when_parent_job_forbids_breakaway() {
    let tender_bin = resolve_bin("TENDER_TEST_BIN", env!("CARGO_BIN_EXE_tender"));
    let helper_bin = resolve_bin(
        "TENDER_TEST_HELPER_BIN",
        env!("CARGO_BIN_EXE_test_breakaway_parent"),
    );
    let home = tempfile::tempdir().expect("tempdir");
    let session = format!("no-breakaway-{}", std::process::id());
    let sidecar_pid_out = home.path().join("sidecar_pid.txt");
    let job_name = format!("tender-test-no-breakaway-{}", std::process::id());

    // KILL_ON_JOB_CLOSE only — explicitly NO BREAKAWAY_OK.
    let job = create_named_job_with_limits(&job_name, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE);

    let status = std::process::Command::new(&helper_bin)
        .arg(&tender_bin)
        .arg(home.path())
        .arg(&session)
        .arg(&sidecar_pid_out)
        .env("TEST_JOB_NAME", &job_name)
        .status()
        .expect("spawn helper");

    let succeeded = status.success();

    // Cleanup before asserting.
    let _ = std::process::Command::new(&tender_bin)
        .env("HOME", home.path())
        .args(["kill", &session, "--force"])
        .status();
    unsafe { TerminateJobObject(job, 1) };
    unsafe { CloseHandle(job) };

    assert!(
        succeeded,
        "tender start should fall back to non-breakaway spawn when the \
         parent's job forbids breakaway. Helper exit: {status:?}"
    );
}
