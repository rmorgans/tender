//! Test fixture: exits with code 42 when it receives CTRL_BREAK.
//!
//! Used by tests/windows_child.rs to verify that graceful kill via
//! GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT) works without escalating
//! to TerminateJobObject.

#[cfg(windows)]
fn main() {
    use std::sync::atomic::{AtomicBool, Ordering};

    static GOT_BREAK: AtomicBool = AtomicBool::new(false);

    unsafe extern "system" fn handler(ctrl_type: u32) -> i32 {
        const CTRL_BREAK_EVENT: u32 = 1;
        if ctrl_type == CTRL_BREAK_EVENT {
            GOT_BREAK.store(true, Ordering::Relaxed);
            return 1; // TRUE — handled, don't terminate
        }
        0 // FALSE — let default handler run
    }

    // SAFETY: handler is a valid extern "system" function with the
    // correct signature for SetConsoleCtrlHandler. We pass TRUE (1)
    // to add (not remove) the handler.
    unsafe {
        windows_sys::Win32::System::Console::SetConsoleCtrlHandler(Some(handler), 1);
    }

    // Poll until CTRL_BREAK is received.
    while !GOT_BREAK.load(Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    std::process::exit(42);
}

#[cfg(not(windows))]
fn main() {
    // Stub for non-Windows builds.
}
