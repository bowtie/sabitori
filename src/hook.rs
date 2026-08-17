use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;
#[cfg(test)]
use std::time::Duration;

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx, HHOOK,
    MSLLHOOKSTRUCT, WH_MOUSE_LL, WM_LBUTTONDOWN, WM_MBUTTONDOWN, WM_MOUSEWHEEL, WM_QUIT,
    WM_RBUTTONDOWN,
};

use crate::config::WheelMode;
use crate::log;

/// Marker injected into `dwExtraInfo` of synthetic events so the hook can
/// recognise and ignore its own injected input, preventing feedback loops.
const SYNTHETIC_MARKER: usize = 0x53_41_42_49_54; // "SABIT"

/// Consecutive reverse ticks required to flip the direction lock. A single
/// opposite tick is treated as wheel bounce; only a sustained run proves the
/// user actually reversed direction.
const REVERSE_TICKS_TO_FLIP: u32 = 2;

/// Status of the low-level mouse hook installation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookStatus {
    /// Hook installation has not completed yet.
    Pending,
    /// Hook installed successfully and is processing input.
    Installed,
    /// Hook installation failed.
    Failed,
}

impl HookStatus {
    /// Encode as `i32` for atomic storage.
    fn as_i32(self) -> i32 {
        match self {
            Self::Pending => 0,
            Self::Installed => 1,
            Self::Failed => 2,
        }
    }

    /// Decode from `i32`; unknown values fall back to `Pending`.
    fn from_i32(v: i32) -> Self {
        match v {
            1 => Self::Installed,
            2 => Self::Failed,
            _ => Self::Pending,
        }
    }
}

/// Thread-safe shared state between the hook thread and the GUI thread.
/// All fields are lock-free atomics so the hook callback never blocks.
#[derive(Debug)]
pub struct SharedState {
    /// Wheel mode as integer (see `WheelMode::as_i32`).
    wheel_mode: AtomicI32,
    /// Direction-lock idle timeout in milliseconds (Mode B).
    dir_lock_timeout_ms: AtomicU32,
    /// Whether the low-level mouse hook is installed (see `HookStatus`).
    hook_status: AtomicI32,
    /// The settings window's on-screen rectangle in physical pixels while the
    /// window is open, or `RECT_CLOSED` in `settings_rect_left` when it is
    /// not. Written by the GUI thread when the window opens/closes; read by
    /// the hook callback to detect outside clicks (native flyout dismissal).
    /// Kept as plain atomics (no mutex) per the lock-free contract above.
    settings_rect_left: AtomicI32,
    settings_rect_top: AtomicI32,
    settings_rect_right: AtomicI32,
    settings_rect_bottom: AtomicI32,
    /// Set by the hook callback when a mouse button-down lands outside the
    /// open settings window; consumed (cleared) once by the GUI poll.
    dismiss_requested: AtomicBool,
    /// Set by the settings window's Retry button when the hook has failed to
    /// install; consumed (cleared) once by the GUI poll, which respawns the
    /// hook thread. The failed thread released the hook-thread slot before
    /// signalling Failed, so a respawn can never collide with it.
    retry_requested: AtomicBool,
}

impl SharedState {
    pub fn new(wheel_mode: WheelMode, dir_lock_timeout_ms: u32) -> Arc<Self> {
        Arc::new(Self {
            wheel_mode: AtomicI32::new(wheel_mode.as_i32()),
            dir_lock_timeout_ms: AtomicU32::new(dir_lock_timeout_ms),
            hook_status: AtomicI32::new(HookStatus::Pending.as_i32()),
            settings_rect_left: AtomicI32::new(RECT_CLOSED),
            settings_rect_top: AtomicI32::new(0),
            settings_rect_right: AtomicI32::new(0),
            settings_rect_bottom: AtomicI32::new(0),
            dismiss_requested: AtomicBool::new(false),
            retry_requested: AtomicBool::new(false),
        })
    }

    pub fn wheel_mode(&self) -> WheelMode {
        WheelMode::from_i32(self.wheel_mode.load(Ordering::Relaxed))
    }

    pub fn set_wheel_mode(&self, mode: WheelMode) {
        self.wheel_mode.store(mode.as_i32(), Ordering::Relaxed);
    }

    pub fn dir_lock_timeout_ms(&self) -> u32 {
        self.dir_lock_timeout_ms.load(Ordering::Relaxed)
    }

    pub fn set_dir_lock_timeout_ms(&self, val: u32) {
        self.dir_lock_timeout_ms.store(val, Ordering::Relaxed);
    }

    pub fn hook_status(&self) -> HookStatus {
        HookStatus::from_i32(self.hook_status.load(Ordering::Relaxed))
    }

    pub fn set_hook_status(&self, status: HookStatus) {
        self.hook_status.store(status.as_i32(), Ordering::Relaxed);
    }

    /// Publish the settings window's on-screen rect (physical pixels) so the
    /// hook can dismiss the flyout on outside clicks. `None` closes it.
    pub fn set_settings_rect(&self, rect: Option<(i32, i32, i32, i32)>) {
        match rect {
            Some((l, t, r, b)) => {
                self.settings_rect_left.store(l, Ordering::Relaxed);
                self.settings_rect_top.store(t, Ordering::Relaxed);
                self.settings_rect_right.store(r, Ordering::Relaxed);
                self.settings_rect_bottom.store(b, Ordering::Relaxed);
            }
            None => self.settings_rect_left.store(RECT_CLOSED, Ordering::Relaxed),
        }
    }

    /// The settings window's current on-screen rect, or `None` when closed.
    fn settings_rect(&self) -> Option<(i32, i32, i32, i32)> {
        let left = self.settings_rect_left.load(Ordering::Relaxed);
        if left == RECT_CLOSED {
            None
        } else {
            Some((
                left,
                self.settings_rect_top.load(Ordering::Relaxed),
                self.settings_rect_right.load(Ordering::Relaxed),
                self.settings_rect_bottom.load(Ordering::Relaxed),
            ))
        }
    }

    /// Ask the GUI to dismiss the settings window (outside click).
    fn request_dismiss(&self) {
        self.dismiss_requested.store(true, Ordering::Relaxed);
    }

    /// Consume a pending dismissal request, if any.
    pub fn take_dismiss_requested(&self) -> bool {
        self.dismiss_requested.swap(false, Ordering::Relaxed)
    }

    /// Ask the GUI to retry installing the mouse hook (settings window's
    /// Retry button, shown when the hook failed).
    pub fn request_retry(&self) {
        self.retry_requested.store(true, Ordering::Relaxed);
    }

    /// Consume a pending hook-retry request, if any.
    pub fn take_retry_requested(&self) -> bool {
        self.retry_requested.swap(false, Ordering::Relaxed)
    }
}

/// Sentinel for [`SharedState::settings_rect_left`] meaning the settings
/// window is closed. `i32::MAX` can't be a real screen coordinate.
const RECT_CLOSED: i32 = i32::MAX;

/// True when the point (physical pixels) lies outside the given rectangle
/// (left, top, right, bottom). A click on the boundary counts as inside.
fn point_outside(rect: (i32, i32, i32, i32), x: i32, y: i32) -> bool {
    x < rect.0 || x > rect.2 || y < rect.1 || y > rect.3
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    Up,
    Down,
}

impl Direction {
    /// Positive delta → `Up`, zero or negative → `Down`.
    fn from_delta(delta: i32) -> Self {
        if delta > 0 {
            Direction::Up
        } else {
            Direction::Down
        }
    }
}

/// Direction-lock state for Mode B (wheel debounce).
struct DirLockState {
    locked_direction: Option<Direction>,
    last_event_time: Option<Instant>,
    /// Consecutive opposite-direction ticks seen while locked. A single
    /// reverse tick is almost always wheel bounce (detent overshoot or
    /// free-spin inertia), so the lock only flips after a sustained run of
    /// reverse ticks, a deliberate direction change.
    reverse_streak: u32,
}

impl DirLockState {
    const fn new() -> Self {
        Self {
            locked_direction: None,
            last_event_time: None,
            reverse_streak: 0,
        }
    }

    /// Reset to the initial state (no locked direction, no timer).
    fn reset(&mut self) {
        *self = Self::new();
    }
}

// Static mutable state accessed only from the hook callback.
// Accessed via raw pointers to avoid creating references to mutable statics
// (Rust 2024 compatibility: static_mut_refs lint).
static mut DIR_LOCK: DirLockState = DirLockState::new();
static SHARED: OnceLock<Arc<SharedState>> = OnceLock::new();

/// Hard guard for the exactly-one-hook-thread invariant. The `static mut`
/// state above is valid only while the single hook thread that owns it is
/// alive. This flag is set by that thread at the very start of its entry,
/// cleared on every exit path, including *before* it signals `Failed` to the
/// GUI, so the status==Failed → respawn retry flow can never be dropped, and
/// checked by the low-level callback before it touches any static mut. If the
/// callback ever finds it unset, the failure mode is log-and-pass-through,
/// never a panic: unwinding out of an OS callback across the FFI boundary is
/// undefined behaviour.
static HOOK_THREAD_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Read-only check: is a hook thread currently alive? The spawn path uses
/// this to refuse starting a second hook thread, which would install a second
/// `WH_MOUSE_LL` hook and double every scroll.
pub fn hook_thread_active() -> bool {
    HOOK_THREAD_ACTIVE.load(Ordering::Acquire)
}

/// Authoritatively claim the hook-thread slot. Called at the very start of
/// the hook thread's entry; at most one thread can hold the slot. The
/// compare-exchange is the hard barrier, a lost race between the spawn
/// path's read-only check and this claim is caught here, and the loser stands
/// down without installing anything. Acquire pairs with the Release store in
/// [`release_hook_thread`].
pub fn try_claim_hook_thread() -> bool {
    HOOK_THREAD_ACTIVE
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_ok()
}

/// Release the hook-thread slot. Release ordering publishes everything the
/// thread did before the clear to whoever observes the flag cleared (the
/// spawn path's Acquire load, or the callback's check).
pub fn release_hook_thread() {
    HOOK_THREAD_ACTIVE.store(false, Ordering::Release);
}

/// Extract the signed wheel delta from the high word of mouseData.
fn extract_wheel_delta(mouse_data: u32) -> i32 {
    i32::from(((mouse_data >> 16) & 0xFFFF) as i16)
}

/// Pure direction-lock decision: returns true if the wheel event should be
/// allowed through, false to swallow. Takes the lock state and a caller-
/// provided clock so tests can drive the idle timeout deterministically; the
/// hook thread calls it via [`dir_lock_filter`] with `Instant::now()`.
///
/// The lock holds whichever direction started the current stroke. An isolated
/// opposite-direction tick is treated as wheel bounce and swallowed; only a
/// sustained run of reverse ticks, a deliberate direction change, flips the
/// lock. The lock also releases after the wheel has been fully idle for
/// `timeout_ms`, so a reversal after a real pause takes effect immediately.
fn dir_lock_filter_logic(
    state: &mut DirLockState,
    delta: i32,
    timeout_ms: u32,
    now: Instant,
) -> bool {
    let event_dir = Direction::from_delta(delta);

    // Release the lock after the wheel has been fully idle for longer than
    // the timeout, so a fresh stroke in either direction starts immediately.
    if let Some(last) = state.last_event_time {
        if now.duration_since(last).as_millis() as u32 >= timeout_ms {
            state.reset();
        }
    }

    match state.locked_direction {
        None => {
            state.locked_direction = Some(event_dir);
            state.last_event_time = Some(now);
            state.reverse_streak = 0;
            true
        }
        Some(locked) => {
            if locked == event_dir {
                // Same direction: re-arm the idle timer and clear any bounce
                // streak accumulated so far.
                state.last_event_time = Some(now);
                state.reverse_streak = 0;
                true
            } else {
                // Opposite direction while locked: count it, but only flip the
                // lock once a sustained run of reverse ticks proves the user
                // actually reversed. Isolated ticks (bounce) are swallowed and
                // leave the lock and its timestamp untouched, so a continuous
                // reverse bounce is fully suppressed even when the wheel's
                // ticks are slower than the timeout.
                state.reverse_streak += 1;
                if state.reverse_streak >= REVERSE_TICKS_TO_FLIP {
                    state.locked_direction = Some(event_dir);
                    state.last_event_time = Some(now);
                    state.reverse_streak = 0;
                    true
                } else {
                    false
                }
            }
        }
    }
}

/// Mode B direction-lock filtering. Runtime entry point: applies
/// [`dir_lock_filter_logic`] to the static lock state with the current clock.
unsafe fn dir_lock_filter(delta: i32, timeout_ms: u32) -> bool {
    // SAFETY: only called from the hook callback, which checks
    // HOOK_THREAD_ACTIVE before reaching here.
    dir_lock_filter_logic(&mut *std::ptr::addr_of_mut!(DIR_LOCK), delta, timeout_ms, Instant::now())
}

/// The low-level mouse hook callback.
unsafe extern "system" fn hook_callback(
    code: i32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if code < 0 {
        return CallNextHookEx(None, code, wparam, lparam);
    }

    // The exactly-one-hook-thread invariant: the static-mut state below is
    // owned by the hook thread and valid only while it is alive. If the flag
    // is ever unset, log and pass the event through untouched. Never panic:
    // unwinding out of an OS callback across the FFI boundary is UB.
    if !HOOK_THREAD_ACTIVE.load(Ordering::Acquire) {
        log::log("hook_callback: no active hook thread; passing event through");
        return CallNextHookEx(None, code, wparam, lparam);
    }

    let info = &*(lparam.0 as *const MSLLHOOKSTRUCT);
    let msg = wparam.0 as u32;

    // Ignore our own synthetic events to prevent feedback loops.
    if info.dwExtraInfo == SYNTHETIC_MARKER {
        return CallNextHookEx(None, code, wparam, lparam);
    }

    let shared = SHARED.get();

    // --- Native flyout dismissal: a mouse button-down outside the open
    // settings window closes it (like the volume/network flyouts). This
    // works even over fullscreen apps, where activation-based dismissal
    // can't fire (the flyout may never hold foreground). The window's rect
    // is published by the GUI thread; the flag is consumed by the GUI poll.
    if matches!(msg, WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN) {
        if let Some(s) = shared {
            if s
                .settings_rect()
                .is_some_and(|rect| point_outside(rect, info.pt.x, info.pt.y))
            {
                s.request_dismiss();
            }
        }
    }

    // --- Wheel handling (filtered vs pass-through) ---
    if msg == WM_MOUSEWHEEL {
        let mode = shared.map_or(WheelMode::DirectionLock, |s| s.wheel_mode());
        match mode {
            WheelMode::Disable => {
                return LRESULT(1);
            }
            WheelMode::DirectionLock => {
                let delta = extract_wheel_delta(info.mouseData);
                if delta == 0 {
                    return CallNextHookEx(None, code, wparam, lparam);
                }
                let timeout = shared.map_or(500, |s| s.dir_lock_timeout_ms());
                let allowed = dir_lock_filter(delta, timeout);

                if allowed {
                    return CallNextHookEx(None, code, wparam, lparam);
                }
                return LRESULT(1);
            }
            WheelMode::Off => {
                return CallNextHookEx(None, code, wparam, lparam);
            }
        }
    }

    CallNextHookEx(None, code, wparam, lparam)
}

/// Install the low-level mouse hook on the current thread.
/// The caller must run a Win32 message pump on this same thread.
pub fn install_hook(shared: Arc<SharedState>) -> Result<HHOOK, String> {
    let _ = SHARED.set(shared);

    unsafe {
        let hook = SetWindowsHookExW(WH_MOUSE_LL, Some(hook_callback), None, 0)
            .map_err(|e| format!("SetWindowsHookExW failed: {e}"))?;
        Ok(hook)
    }
}

/// Remove the hook.
pub fn remove_hook(hook: HHOOK) {
    unsafe {
        let _ = UnhookWindowsHookEx(hook);
    }
}

/// Run a standard Win32 message pump on the current thread.
/// Returns when `WM_QUIT` is received.
pub fn run_message_loop() {
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, TranslateMessage, MSG,
    };

    unsafe {
        let mut msg = MSG::default();
        loop {
            if !GetMessageW(&mut msg, None, 0, 0).as_bool() {
                break;
            }

            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Returns the OS thread id of the calling thread (call from the hook thread).
pub fn current_thread_id() -> u32 {
    unsafe { GetCurrentThreadId() }
}

/// Posts `WM_QUIT` to the hook thread so its message loop exits.
/// Returns true if the message was posted successfully.
pub fn post_quit(thread_id: u32) -> bool {
    unsafe { PostThreadMessageW(thread_id, WM_QUIT, WPARAM(0), LPARAM(0)).is_ok() }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: feed a sequence of deltas at the given timestamps and collect
    /// the allow/swallow decisions.
    fn run_sequence(
        deltas: &[i32],
        timestamps: &[Instant],
        timeout_ms: u32,
    ) -> Vec<bool> {
        assert_eq!(deltas.len(), timestamps.len());
        let mut state = DirLockState::new();
        deltas
            .iter()
            .zip(timestamps)
            .map(|(&d, &t)| dir_lock_filter_logic(&mut state, d, timeout_ms, t))
            .collect()
    }

    /// Helper: generate timestamps at fixed intervals starting from a base.
    fn timestamps(base: Instant, interval_ms: u64, count: usize) -> Vec<Instant> {
        (0..count)
            .map(|i| base + Duration::from_millis(interval_ms * i as u64))
            .collect()
    }

    #[test]
    fn first_event_always_allowed() {
        let t0 = Instant::now();
        let result = run_sequence(&[120], &[t0], 500);
        assert_eq!(result, vec![true]);
    }

    #[test]
    fn same_direction_all_allowed() {
        let t0 = Instant::now();
        let ts = timestamps(t0, 10, 5);
        let result = run_sequence(&[120, 120, 120, 120, 120], &ts, 500);
        assert_eq!(result, vec![true, true, true, true, true]);
    }

    #[test]
    fn single_opposite_tick_swallowed() {
        let t0 = Instant::now();
        let ts = timestamps(t0, 10, 3);
        // Up, Up, Down, the single Down is bounce, swallowed.
        let result = run_sequence(&[120, 120, -120], &ts, 500);
        assert_eq!(result, vec![true, true, false]);
    }

    #[test]
    fn two_consecutive_opposite_ticks_flip_lock() {
        let t0 = Instant::now();
        let ts = timestamps(t0, 10, 3);
        // Up, Down, Down, first Down swallowed, second Down flips and allows.
        let result = run_sequence(&[120, -120, -120], &ts, 500);
        assert_eq!(result, vec![true, false, true]);
    }

    #[test]
    fn bounce_streak_resets_on_same_direction() {
        let t0 = Instant::now();
        let ts = timestamps(t0, 10, 4);
        // Up, Down(bounce), Up(clears streak), Down(bounce again)
        let result = run_sequence(&[120, -120, 120, -120], &ts, 500);
        assert_eq!(result, vec![true, false, true, false]);
    }

    #[test]
    fn idle_timeout_releases_lock() {
        let t0 = Instant::now();
        // Scroll up, then wait past the timeout, then scroll down.
        // The lock should have released, so the Down is allowed.
        let ts = vec![t0, t0 + Duration::from_millis(600)];
        let result = run_sequence(&[120, -120], &ts, 500);
        assert_eq!(result, vec![true, true]);
    }

    #[test]
    fn idle_within_timeout_keeps_lock() {
        let t0 = Instant::now();
        // Scroll up, then just under the timeout, scroll down, still locked.
        let ts = vec![t0, t0 + Duration::from_millis(499)];
        let result = run_sequence(&[120, -120], &ts, 500);
        assert_eq!(result, vec![true, false]);
    }

    #[test]
    fn idle_exactly_at_timeout_releases_lock() {
        let t0 = Instant::now();
        // Scroll up, then wait exactly the timeout, scroll down.
        // The comparison is >=, so exactly 500ms releases the lock.
        let ts = vec![t0, t0 + Duration::from_millis(500)];
        let result = run_sequence(&[120, -120], &ts, 500);
        assert_eq!(result, vec![true, true]);
    }

    #[test]
    fn continuous_reverse_bounce_suppressed() {
        let t0 = Instant::now();
        let ts = timestamps(t0, 100, 6);
        // Up, then alternating Down/Up with 100ms gaps (under 500ms timeout).
        // The single Down ticks never reach 2 consecutive, so all Down are
        // swallowed. The Up ticks are same-direction and allowed.
        let result = run_sequence(&[120, -120, 120, -120, 120, -120], &ts, 500);
        assert_eq!(result, vec![true, false, true, false, true, false]);
    }

    #[test]
    fn lock_flips_then_new_direction_dominates() {
        let t0 = Instant::now();
        let ts = timestamps(t0, 10, 5);
        // Up, Down, Down (flips to Down), Down (same dir, allowed), Down
        let result = run_sequence(&[120, -120, -120, -120, -120], &ts, 500);
        assert_eq!(result, vec![true, false, true, true, true]);
    }

    #[test]
    fn zero_delta_passes_through() {
        // A zero delta maps to Direction::Down (delta <= 0), but the logic
        // should still process it. The first event is always allowed.
        let t0 = Instant::now();
        let result = run_sequence(&[0], &[t0], 500);
        assert_eq!(result, vec![true]);
    }

    #[test]
    fn extract_wheel_delta_positive() {
        // WHEEL_DELTA = 120 → high word = 120 → delta = +120.
        assert_eq!(extract_wheel_delta(120 << 16), 120);
    }

    #[test]
    fn extract_wheel_delta_negative() {
        // -120 as i16 → high word = 0xFF88 → delta = -120.
        let mouse_data = ((-120i16) as u16 as u32) << 16;
        assert_eq!(extract_wheel_delta(mouse_data), -120);
    }

    #[test]
    fn extract_wheel_delta_large_negative() {
        // -1000 as i16 → high word = 0xFC18 → delta = -1000.
        let mouse_data = ((-1000i16) as u16 as u32) << 16;
        assert_eq!(extract_wheel_delta(mouse_data), -1000);
    }

    #[test]
    fn point_outside_check() {
        let rect = (100, 100, 200, 200);
        assert!(!point_outside(rect, 150, 150)); // center
        assert!(!point_outside(rect, 100, 100)); // top-left corner
        assert!(point_outside(rect, 99, 150)); // left of rect
        assert!(point_outside(rect, 150, 201)); // below rect
        assert!(point_outside(rect, 201, 150)); // right of rect
        assert!(point_outside(rect, 150, 99)); // above rect
    }

    #[test]
    fn hook_status_roundtrip() {
        for status in [HookStatus::Pending, HookStatus::Installed, HookStatus::Failed] {
            assert_eq!(HookStatus::from_i32(status.as_i32()), status);
        }
    }

    #[test]
    fn hook_status_from_i32_fallback() {
        // Unknown values fall back to Pending.
        assert_eq!(HookStatus::from_i32(999), HookStatus::Pending);
        assert_eq!(HookStatus::from_i32(-1), HookStatus::Pending);
    }

    #[test]
    fn shared_state_settings_rect_roundtrip() {
        let shared = SharedState::new(WheelMode::DirectionLock, 500);
        // Starts closed.
        assert!(shared.settings_rect().is_none());
        // Set and read back.
        shared.set_settings_rect(Some((100, 200, 300, 400)));
        assert_eq!(shared.settings_rect(), Some((100, 200, 300, 400)));
        // Close again.
        shared.set_settings_rect(None);
        assert!(shared.settings_rect().is_none());
    }

    #[test]
    fn shared_state_dismiss_requested_consumed_once() {
        let shared = SharedState::new(WheelMode::DirectionLock, 500);
        assert!(!shared.take_dismiss_requested());
        shared.request_dismiss();
        assert!(shared.take_dismiss_requested());
        // Second take returns false, the flag was consumed.
        assert!(!shared.take_dismiss_requested());
    }

    #[test]
    fn shared_state_retry_requested_consumed_once() {
        let shared = SharedState::new(WheelMode::DirectionLock, 500);
        assert!(!shared.take_retry_requested());
        shared.request_retry();
        assert!(shared.take_retry_requested());
        // Second take returns false, the flag was consumed.
        assert!(!shared.take_retry_requested());
    }

    #[test]
    fn shared_state_wheel_mode_roundtrip() {
        let shared = SharedState::new(WheelMode::DirectionLock, 500);
        assert_eq!(shared.wheel_mode(), WheelMode::DirectionLock);
        shared.set_wheel_mode(WheelMode::Off);
        assert_eq!(shared.wheel_mode(), WheelMode::Off);
        shared.set_wheel_mode(WheelMode::Disable);
        assert_eq!(shared.wheel_mode(), WheelMode::Disable);
        shared.set_wheel_mode(WheelMode::DirectionLock);
        assert_eq!(shared.wheel_mode(), WheelMode::DirectionLock);
    }

    #[test]
    fn shared_state_dir_lock_timeout_roundtrip() {
        let shared = SharedState::new(WheelMode::DirectionLock, 500);
        assert_eq!(shared.dir_lock_timeout_ms(), 500);
        shared.set_dir_lock_timeout_ms(250);
        assert_eq!(shared.dir_lock_timeout_ms(), 250);
        shared.set_dir_lock_timeout_ms(2000);
        assert_eq!(shared.dir_lock_timeout_ms(), 2000);
    }

    #[test]
    fn shared_state_hook_status_roundtrip() {
        let shared = SharedState::new(WheelMode::DirectionLock, 500);
        assert_eq!(shared.hook_status(), HookStatus::Pending);
        shared.set_hook_status(HookStatus::Installed);
        assert_eq!(shared.hook_status(), HookStatus::Installed);
        shared.set_hook_status(HookStatus::Failed);
        assert_eq!(shared.hook_status(), HookStatus::Failed);
    }

    #[test]
    fn direction_from_delta() {
        assert_eq!(Direction::from_delta(120), Direction::Up);
        assert_eq!(Direction::from_delta(1), Direction::Up);
        assert_eq!(Direction::from_delta(-120), Direction::Down);
        assert_eq!(Direction::from_delta(0), Direction::Down);
    }
}
