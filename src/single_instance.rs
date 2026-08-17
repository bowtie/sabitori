//! Single-instance guard: only one Sabitori process may run at a time.
//!
//! A named mutex is created at startup and held for the process lifetime. A
//! second launch finds the mutex already exists, writes a clear log line, and
//! exits before installing a hook, creating a tray icon, or opening any
//! window, two processes would each install a `WH_MOUSE_LL` hook and react
//! to the same M3 drag, doubling the scroll.
//!
//! As a convenience, the second launch also signals a named event that the
//! running instance's 50ms poll loop watches, so the running instance brings
//! its settings window forward instead of ignoring the second launch.

use std::sync::OnceLock;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, ResetEvent, SetEvent, WaitForSingleObject,
};

use crate::wide::to_wide;

/// Per-session names (`Local\`), so two users on one machine each get their
/// own instance, correct, since the tray icon and hook are per-session.
const MUTEX_NAME: &str = "Local\\Sabitori.SingleInstance.Mutex";
const SECOND_LAUNCH_EVENT_NAME: &str = "Local\\Sabitori.SecondLaunch.Event";

/// A kernel handle that lives for the whole process. Kernel handles are plain
/// pointers and are safe to use from any thread, so sharing the event handle
/// with the poll loop (a different thread) is sound; the handle is never
/// closed while the process runs.
#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
struct KernelHandle(HANDLE);
unsafe impl std::marker::Send for KernelHandle {}
unsafe impl std::marker::Sync for KernelHandle {}

/// The single-instance mutex and the second-launch signal event, both created
/// once at startup and held for the process lifetime.
static MUTEX: OnceLock<KernelHandle> = OnceLock::new();
static SECOND_LAUNCH_EVENT: OnceLock<KernelHandle> = OnceLock::new();

/// Create the single-instance mutex (and the second-launch signal event) and
/// report whether this process is the first instance. When another instance
/// is already running, a log line is written and `false` is returned; the
/// caller must exit before touching the hook, tray, or any window.
pub fn acquire() -> bool {
    // The mutex's existence is the authoritative check. `binitialowner: false`
    //, the mutex is never used for exclusion, only as a named sentinel.
    let name = to_wide(MUTEX_NAME);
    let handle = match unsafe { CreateMutexW(None, false, PCWSTR(name.as_ptr())) } {
        Ok(h) => h,
        Err(e) => {
            // Practically unreachable, but fail closed: never risk two hooks.
            crate::log::log(&format!("Failed to create single-instance mutex: {e}"));
            return false;
        }
    };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        crate::log::log("Another Sabitori instance is already running; exiting");
        return false;
    }
    let _ = MUTEX.set(KernelHandle(handle));

    // The event the next launch signals to open this instance's settings
    // window. Best-effort: if it fails the guard still works, the second
    // launch just won't bring the window forward.
    let event_name = to_wide(SECOND_LAUNCH_EVENT_NAME);
    match unsafe { CreateEventW(None, true, false, PCWSTR(event_name.as_ptr())) } {
        Ok(event) => {
            let _ = SECOND_LAUNCH_EVENT.set(KernelHandle(event));
        }
        Err(e) => crate::log::log(&format!("Failed to create second-launch event: {e}")),
    }

    true
}

/// Signal the running instance (from a second launch) to bring its settings
/// window forward. Best-effort: failure is ignored since the second instance
/// is about to exit anyway.
pub fn signal_second_launch() {
    let name = to_wide(SECOND_LAUNCH_EVENT_NAME);
    // Open-or-create: the running instance created the event, so this opens
    // the existing object rather than creating a fresh one.
    if let Ok(event) = unsafe { CreateEventW(None, true, false, PCWSTR(name.as_ptr())) } {
        let _ = unsafe { SetEvent(event) };
        // Close the handle, this process exits immediately after, but
        // closing explicitly is correct and avoids a handle leak.
        let _ = unsafe { CloseHandle(event) };
    }
}

/// Non-blocking check for a second-launch signal. Returns true once per
/// signal; call from the poll loop to open the settings window.
pub fn poll_second_launch() -> bool {
    let Some(event) = SECOND_LAUNCH_EVENT.get() else {
        return false;
    };
    let KernelHandle(handle) = *event;
    if unsafe { WaitForSingleObject(handle, 0) } == WAIT_OBJECT_0 {
        let _ = unsafe { ResetEvent(handle) };
        true
    } else {
        false
    }
}
