use std::borrow::Cow;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder;
use gpui::{
    div, point, px, svg, Animation, AnimationExt, App, AppContext, Application,
    Context, FocusHandle, Hsla, InteractiveElement, IntoElement, KeyDownEvent, ParentElement,
    Pixels, Rgba, Render, SharedString, StatefulInteractiveElement, Styled, Subscription, Window,
    WindowHandle, WindowOptions, ease_out_quint,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE,
    DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
};
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetWindowLongPtrW, GetWindowRect, SetWindowLongPtrW, SetWindowPos,
    SystemParametersInfoW, GWL_EXSTYLE, GWL_STYLE, HWND_TOPMOST, SPI_GETWORKAREA, SWP_FRAMECHANGED,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SWP_NOSIZE, WINDOW_EX_STYLE, WINDOW_STYLE,
    WS_CAPTION, WS_EX_CLIENTEDGE, WS_EX_WINDOWEDGE, WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_SYSMENU,
    WS_THICKFRAME,
};

use crate::config::{Config, WheelMode, DIR_LOCK_TIMEOUT_MAX_MS, DIR_LOCK_TIMEOUT_MIN_MS};
use crate::hook::{HookStatus, SharedState};
use crate::theme;
use gpui_component::button::{
    Button, ButtonCustomVariant, ButtonGroup, ButtonVariants,
};
use gpui_component::checkbox::Checkbox;
use gpui_component::{ActiveTheme, Disableable, Selectable};

// ── Color palette ─────────────────────────────────────────────────────
//
// The settings UI colors are derived from the Windows system theme:
// the accent color is read from the registry, and the surface/border/
// text colors follow the system light/dark mode. The Mica backdrop
// provides the actual window background, so the card surface uses a
// semi-transparent fill that lets Mica show through.

/// The full color palette for the settings UI. Built from the live
/// Windows system accent color and light/dark mode.
#[derive(Clone, Copy)]
#[allow(dead_code)]
struct Theme {
    /// Semi-transparent surface for the settings card, lets Mica show.
    surface: Hsla,
    control_bg: Hsla,
    control_bg_hover: Hsla,
    border: Hsla,
    text_primary: Hsla,
    text_secondary: Hsla,
    text_on_accent: Hsla,
    accent: Hsla,
    error: Hsla,
}

impl Theme {
    /// Build the palette from the Windows system accent color and
    /// light/dark mode.
    fn system() -> Self {
        let want_dark = !theme::is_light_mode();
        let [r, g, b] = theme::accent_color();
        let accent: Hsla = Rgba {
            r: r as f32 / 255.0,
            g: g as f32 / 255.0,
            b: b as f32 / 255.0,
            a: 1.0,
        }
        .into();

        let white = Hsla {
            h: 0.0,
            s: 0.0,
            l: 1.0,
            a: 1.0,
        };

        if want_dark {
            Self {
                // Semi-transparent so the acrylic blur shows through the
                // card itself, frosted glass effect.
                surface: Hsla {
                    h: 0.0,
                    s: 0.0,
                    l: 0.12,
                    a: 0.5,
                },
                control_bg: Hsla {
                    h: 0.0,
                    s: 0.0,
                    l: 0.2,
                    a: 0.8,
                },
                control_bg_hover: Hsla {
                    h: 0.0,
                    s: 0.0,
                    l: 0.28,
                    a: 0.9,
                },
                border: Hsla {
                    h: 0.0,
                    s: 0.0,
                    l: 0.3,
                    a: 0.6,
                },
                text_primary: Hsla {
                    h: 0.0,
                    s: 0.0,
                    l: 0.9,
                    a: 1.0,
                },
                text_secondary: Hsla {
                    h: 0.0,
                    s: 0.0,
                    l: 0.7,
                    a: 1.0,
                },
                text_on_accent: white,
                accent,
                error: Hsla {
                    h: 0.0,
                    s: 0.62,
                    l: 0.45,
                    a: 1.0,
                },
            }
        } else {
            Self {
                surface: Hsla {
                    h: 0.0,
                    s: 0.0,
                    l: 0.98,
                    a: 0.5,
                },
                control_bg: Hsla {
                    h: 0.0,
                    s: 0.0,
                    l: 0.92,
                    a: 0.8,
                },
                control_bg_hover: Hsla {
                    h: 0.0,
                    s: 0.0,
                    l: 0.85,
                    a: 0.9,
                },
                border: Hsla {
                    h: 0.0,
                    s: 0.0,
                    l: 0.78,
                    a: 0.6,
                },
                text_primary: Hsla {
                    h: 0.0,
                    s: 0.0,
                    l: 0.12,
                    a: 1.0,
                },
                text_secondary: Hsla {
                    h: 0.0,
                    s: 0.0,
                    l: 0.3,
                    a: 1.0,
                },
                text_on_accent: white,
                accent,
                error: Hsla {
                    h: 0.0,
                    s: 0.62,
                    l: 0.45,
                    a: 1.0,
                },
            }
        }
    }
}

// ── Spacing scale ─────────────────────────────────────────────────────
//
// Consistent spacing values used throughout the settings UI. Every padding,
// gap, and margin should draw from this scale.
const SP_MD: f32 = 8.0;
const SP_LG: f32 = 12.0;
const SP_XL: f32 = 16.0;

/// Padding of the window content, also used to map click positions back
/// to slider values. Smaller than the original 20 px so the flyout looks
/// like a compact tray popover rather than a framed card.
const CONTENT_PADDING: f32 = SP_XL;

/// The flyout has no extra transparent margin around the content; the
/// DWM-rounded client area is the same size as the rendered layout.
const PANEL_MARGIN: f32 = 0.0;

/// How long after the settings window opens before focus-out dismissals are
/// honored. The flyout is activated programmatically, and Windows can deny
/// that activation (SetForegroundWindow restrictions), firing `on_focus_out`
/// right after open, which would kill the flyout before the user sees it.
/// A deliberate outside click always comes later than this window.
const OPEN_FOCUS_GRACE: Duration = Duration::from_millis(300);

/// Embedded Inter font (SIL OFL-1.1, rsms/inter), registered at startup so
/// the UI renders in Inter on any machine. Bundled because Inter is not a
/// Windows system font and the app ships as one exe.
static INTER_REGULAR: &[u8] = include_bytes!("../assets/fonts/Inter-Regular.ttf");
static INTER_MEDIUM: &[u8] = include_bytes!("../assets/fonts/Inter-Medium.ttf");


/// The tray icon's on-screen rectangle in physical (device) pixels, as
/// reported by Win32 (`Shell_NotifyIconGetRect`). Only the left/top/width
/// matter for flyout placement (the window sits above the icon's top edge,
/// centered on its horizontal extent).
#[derive(Clone, Copy, Debug)]
struct IconRect {
    left: f64,
    top: f64,
    width: u32,
}

impl From<tray_icon::Rect> for IconRect {
    fn from(r: tray_icon::Rect) -> Self {
        Self {
            left: r.position.x,
            top: r.position.y,
            width: r.size.width,
        }
    }
}

/// The primary monitor's work area (desktop excluding the taskbar) in
/// physical pixels.
#[derive(Clone, Copy, Debug, Default)]
struct WorkArea {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

/// Logical-pixel origin that places the window like a native tray flyout:
/// just above the tray icon when its position is known (centered on it
/// horizontally), otherwise just above the taskbar's right edge (the work
/// area's bottom-right corner, near the clock). Clamped to the work area so
/// no layout, auto-hide taskbar, taskbar on an unusual edge, small screens:
/// can push the window off-screen. `icon` and `work` are physical pixels;
/// the result is in logical pixels for GPUI's window bounds.
fn flyout_origin(
    window_size: gpui::Size<Pixels>,
    icon: Option<IconRect>,
    work: WorkArea,
    scale: f32,
) -> gpui::Point<Pixels> {
    const GAP_PX: f32 = 8.0; // gap between the window and the icon/taskbar
    let w: f32 = window_size.width.into();
    let h: f32 = window_size.height.into();

    let (x, y) = match icon {
        Some(icon) => {
            // Center on the icon horizontally, sit just above it.
            let icon_cx = (icon.left + f64::from(icon.width) / 2.0) as f32 / scale;
            let icon_top = icon.top as f32 / scale;
            (icon_cx - w / 2.0, icon_top - h - GAP_PX)
        }
        None => (
            // Bottom-right of the work area, near the clock / tray cluster.
            work.right as f32 / scale - w - GAP_PX,
            work.bottom as f32 / scale - h - GAP_PX,
        ),
    };

    let left = work.left as f32 / scale;
    let top = work.top as f32 / scale;
    let right = work.right as f32 / scale;
    let bottom = work.bottom as f32 / scale;
    point(
        px(x.clamp(left, (right - w).max(left))),
        px(y.clamp(top, (bottom - h).max(top))),
    )
}

/// The client size of the settings flyout, computed from the rows it actually
/// shows so the window hugs its content like a native tray flyout. The width
/// is fixed; the height is the sum of the visible rows plus the vertical
/// padding and the gaps between them. Rows: the failure banner (only when the
/// hook failed), the title row (with the wheel-mode toggle in its top-right
/// corner), the scrolling-timeout stepper (always present, it's just faded
/// and disabled when the toggle is off), the divider, and the
/// Start-with-Windows checkbox. The render and the window sizing share these
/// heights, so they can't drift.
fn settings_window_size(hook_failed: bool) -> gpui::Size<Pixels> {
    // Narrow, minimal panel.
    const WIDTH: f32 = 280.0;
    // Gap between rows, SP_LG gives each feature room to breathe (the
    // card's render uses the same value).
    const GAP: f32 = SP_LG;
    // Top and bottom padding, equal so all four window edges match.
    const PAD: f32 = 2.0 * CONTENT_PADDING;
    // Title row: app name text; the 32x32 toggle button is the tallest
    // element.
    const TITLE_H: f32 = 36.0;
    // Timeout stepper section: centered label line + gap + the 28px-tall
    // stepper row (28px buttons flanking the value chip).
    const TIMEOUT_H: f32 = 20.0 + SP_MD + 28.0;
    // The 1px divider line between the timeout feature and the rows below.
    const DIVIDER_H: f32 = 1.0;
    // Start-with-Windows checkbox row (gpui-component checkbox, ~26px tall
    // including its label line).
    const AUTOSTART_H: f32 = 26.0;
    // Failure banner: SP_MD vertical padding * 2 + tallest child (Retry
    // button at ~24px with SP_XS padding + text) + line spacing.
    const BANNER_H: f32 = 44.0;

    // Accumulate row heights and count without heap allocation. The row
    // set depends only on hook status (the stepper row is always shown), so
    // the maximum is 6 rows.
    let mut row_sum = 0.0_f32;
    let mut row_count = 0_u32;

    if hook_failed {
        row_sum += BANNER_H;
        row_count += 1;
    }
    row_sum += TITLE_H;
    row_count += 1;
    row_sum += TIMEOUT_H;
    row_count += 1;
    row_sum += DIVIDER_H;
    row_count += 1;
    row_sum += AUTOSTART_H;
    row_count += 1;

    // Plus the transparent panel margin on both vertical edges. No extra
    // text-line fudge is needed: the row heights already account for their
    // content, so adding one would leave empty space at the bottom.
    let height = PAD + GAP * (row_count as f32 - 1.0) + row_sum + 2.0 * PANEL_MARGIN;
    gpui::size(px(WIDTH), px(height))
}

/// The primary monitor's work area (desktop minus the taskbar) in physical
/// pixels, via `SPI_GETWORKAREA`. Falls back to all-zeros (which the flyout
/// clamp treats as the top-left corner) if the system call fails, it
/// effectively never does.
fn primary_work_area() -> WorkArea {
    let mut rect = RECT::default();
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut rect as *mut _ as *mut _),
            windows::Win32::UI::WindowsAndMessaging::SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
    }
    WorkArea {
        left: rect.left,
        top: rect.top,
        right: rect.right,
        bottom: rect.bottom,
    }
}

/// Scale factor of the primary display, used to convert physical-pixel
/// positions from Win32 into GPUI logical pixels. Falls back to 1.0 (no
/// scaling) if the DPI query fails, it effectively never does.
fn system_scale() -> f32 {
    let dpi = unsafe { GetDpiForSystem() };
    if dpi == 0 {
        1.0
    } else {
        dpi as f32 / 96.0
    }
}

/// Override the acrylic blur tint color. GPUI's `Blurred` mode sets acrylic
/// with a near-transparent tint `(0,0,0,0)`. This re-applies
/// `SetWindowCompositionAttribute` with `ACCENT_ENABLE_ACRYLICBLURBEHIND`
/// (state 4) and a darker tint so the window has a dark frosted glass
/// backing without needing a card background fill.
fn set_acrylic_tint(hwnd: HWND, want_dark: bool) {
    #[repr(C)]
    struct AccentPolicy {
        accent_state: u32,
        accent_flags: u32,
        gradient_color: u32,
        animation_id: u32,
    }
    #[repr(C)]
    struct WindowCompositionAttribData {
        attrib: u32,
        pv_data: *mut std::ffi::c_void,
        cb_data: usize,
    }
    type SetWindowCompositionAttributeFn =
        unsafe extern "system" fn(HWND, *mut WindowCompositionAttribData) -> i32;

    // Acrylic tint: (r, g, b, a) packed as a u32 in 0xAABBGGRR format.
    // Dark: a dark gray at ~60% opacity. Light: a light gray at ~60%.
    let (r, g, b, a) = if want_dark {
        (30u8, 30u8, 34u8, 160u8)
    } else {
        (245u8, 245u8, 248u8, 160u8)
    };
    let gradient_color = (r as u32) | ((g as u32) << 8) | ((b as u32) << 16) | ((a as u32) << 24);

    unsafe {
        let module_name = windows::core::PCSTR::from_raw(c"user32.dll".as_ptr() as *const u8);
        let user32 = match windows::Win32::System::LibraryLoader::GetModuleHandleA(module_name) {
            Ok(h) => h,
            Err(_) => return,
        };
        let func_name = windows::core::PCSTR::from_raw(
            c"SetWindowCompositionAttribute".as_ptr() as *const u8,
        );
        let func: Option<SetWindowCompositionAttributeFn> =
            windows::Win32::System::LibraryLoader::GetProcAddress(user32, func_name)
                .map(|p| std::mem::transmute(p));
        let Some(set_attr) = func else { return };

        let accent = AccentPolicy {
            accent_state: 4, // ACCENT_ENABLE_ACRYLICBLURBEHIND
            accent_flags: 0,
            gradient_color,
            animation_id: 0,
        };
        let mut data = WindowCompositionAttribData {
            attrib: 0x13, // WCA_ACCENTPOLICY
            pv_data: &accent as *const _ as *mut _,
            cb_data: std::mem::size_of::<AccentPolicy>(),
        };
        let _ = set_attr(hwnd, &mut data as *mut _);
    }
}

/// Which numeric slider the user is interacting with.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SliderKind {
    Timeout,
}

impl SliderKind {
    fn label(self) -> &'static str {
        match self {
            Self::Timeout => "Scroll timeout",
        }
    }

    /// Arrow-key nudge step, in the slider's native units.
    fn nudge_step(self) -> i32 {
        match self {
            Self::Timeout => 5, // 5 ms per press
        }
    }

    /// Valid value range, in the slider's native units.
    fn nudge_range(self) -> (i32, i32) {
        match self {
            Self::Timeout => (DIR_LOCK_TIMEOUT_MIN_MS as i32, DIR_LOCK_TIMEOUT_MAX_MS as i32),
        }
    }
}


/// State for the settings window view.
pub struct SettingsView {
    shared: Arc<SharedState>,
    /// Focus handle for the window's root element. In GPUI 0.2.2, keyboard
    /// events only reach elements on the focus path, so the root div must be
    /// focused for its `on_key_down` handler (typing, arrow nudging, Tab,
    /// Escape) to fire at all.
    focus_handle: FocusHandle,
    /// Local mirror of wheel mode.
    wheel_mode: WheelMode,
    /// Local mirror of direction-lock timeout.
    dir_lock_timeout_ms: u32,
    /// Resolved color palette (from Windows system accent + light/dark).
    theme: Theme,
    /// Local mirror of the autostart-on-login toggle.
    autostart: bool,
    /// Local mirror of the mouse hook status, so a live status change can
    /// trigger a re-render of the warning banner.
    hook_status: HookStatus,
    /// Which value is being typed directly (if any).
    editing: Option<SliderKind>,
    /// Which slider currently has keyboard focus, so arrow keys can nudge it.
    focused_slider: Option<SliderKind>,
    /// Text buffer for the value currently being typed.
    edit_buffer: String,
    /// Keeps the focus-out dismiss listener alive for as long as the window
    /// is open; dropping it (when the view drops) unregisters the listener.
    focus_out_sub: Option<Subscription>,
    /// Native HWND of this window, for the content-height resize and the
    /// rounded-corner region. Captured once at open (the raw handle is only
    /// reachable through `&mut Window`, which the refresh poll doesn't have).
    hwnd: isize,
    /// Current client height in logical pixels, tracked so the refresh poll
    /// can grow/shrink the window when the visible rows change (hook failure
    /// banner) while keeping the bottom edge anchored to the tray.
    client_h: f32,
    /// Callback to save config when settings change.
    on_config_change: Box<dyn Fn(&Config)>,
    /// When we last checked the registry for autostart status (Unix seconds).
    /// Throttles the registry read to once per second.
    last_autostart_check: Option<u64>,
}

impl SettingsView {
    /// Create the settings view, seeding local mirrors from shared state
    /// and spawning the 50ms refresh poll that keeps the window in sync
    /// with external changes (tray reset, hook retry, autostart registry).
    pub fn new(
        shared: Arc<SharedState>,
        on_config_change: Box<dyn Fn(&Config)>,
        cx: &mut Context<Self>,
    ) -> Self {
        // Poll shared state so external changes (e.g. a tray "Reset to
        // Defaults") are reflected in an already-open window. The task holds
        // a weak handle to the view, so it stops once the window is closed.
        // A change can also alter the visible rows (wheel mode, hook failure
        // banner), so the poll resizes the native window to hug the content.
        cx.spawn(async move |this, cx| {
            loop {
                gpui::Timer::after(Duration::from_millis(50)).await;
                // `update` fails once the view has been dropped, which is
                // our signal to stop polling.
                let alive = this.update(cx, |view, cx| {
                    if view.refresh_from_shared() {
                        view.resize_to_fit();
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();

        let theme = Theme::system();

        Self {
            focus_handle: cx.focus_handle(),
            wheel_mode: shared.wheel_mode(),
            dir_lock_timeout_ms: shared.dir_lock_timeout_ms(),
            theme,
            autostart: crate::autostart::is_enabled(),
            hook_status: shared.hook_status(),
            editing: None,
            focused_slider: None,
            edit_buffer: String::new(),
            focus_out_sub: None,
            hwnd: 0,
            client_h: 0.0,
            shared,
            on_config_change,
            last_autostart_check: None,
        }
    }

    /// Grow or shrink the native window so it hugs the visible rows (wheel
    /// mode, hook failure banner), keeping the bottom edge anchored to the
    /// tray like a native flyout. No-op when the height is already correct.
    /// Also republishes the on-screen rect so the hook's outside-click
    /// dismissal tracks the new bounds.
    fn resize_to_fit(&mut self) {
        let target = settings_window_size(self.hook_status == HookStatus::Failed);
        let target_h: f32 = target.height.into();
        if (target_h - self.client_h).abs() < 1.0 {
            return;
        }
        let hwnd = HWND(self.hwnd as *mut _);
        unsafe {
            let mut rc = RECT::default();
            if GetWindowRect(hwnd, &mut rc).is_ok() {
                let delta = target_h - self.client_h;
                let new_h = (rc.bottom - rc.top) + delta as i32;
                let new_top = rc.bottom - new_h; // keep the bottom edge fixed
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    rc.left,
                    new_top,
                    rc.right - rc.left,
                    new_h,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                self.client_h = target_h;
                let mut rc2 = RECT::default();
                if GetWindowRect(hwnd, &mut rc2).is_ok() {
                    self.shared
                        .set_settings_rect(Some((rc2.left, rc2.top, rc2.right, rc2.bottom)));
                }
            }
        }
    }

    /// Bring this window forward and put keyboard focus on its root element.
    /// Used when the settings window is already open and the user clicks the
    /// tray icon (or the tray Settings item) again, instead of opening a
    /// duplicate window.
    pub fn focus_window(&self, window: &mut Window) {
        self.focus_handle.focus(window);
    }

    /// Re-read all settings from shared state, returning true if anything
    /// changed. Used to reflect external changes (such as a tray reset) in an
    /// already-open window without re-saving the config.
    fn refresh_from_shared(&mut self) -> bool {
        let mut changed = false;

        let wheel_mode = self.shared.wheel_mode();
        if wheel_mode != self.wheel_mode {
            self.wheel_mode = wheel_mode;
            changed = true;
        }

        let dir_lock_timeout_ms = self.shared.dir_lock_timeout_ms();
        if dir_lock_timeout_ms != self.dir_lock_timeout_ms {
            self.dir_lock_timeout_ms = dir_lock_timeout_ms;
            changed = true;
        }

        // The hook status isn't a user setting, but the warning banner depends
        // on it, so track it to re-render when a retry succeeds or fails.
        let hook_status = self.shared.hook_status();
        if hook_status != self.hook_status {
            self.hook_status = hook_status;
            changed = true;
        }

        // Refresh autostart from the registry so external changes (e.g. the
        // user disabling it via Task Manager's startup tab) are reflected
        // while the settings window is open. Throttled to once per second
        // since registry access is heavier than atomic reads and the setting
        // rarely changes.
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if self.last_autostart_check.is_none_or(|s| now_secs.saturating_sub(s) >= 1) {
            self.last_autostart_check = Some(now_secs);
            let autostart = crate::autostart::is_enabled();
            if autostart != self.autostart {
                self.autostart = autostart;
                changed = true;
            }
        }

        changed
    }

    fn current_config(&self) -> Config {
        Config {
            wheel_mode: self.wheel_mode,
            direction_lock_timeout_ms: self.dir_lock_timeout_ms,
        }
    }

    fn save_and_sync(&mut self) {
        self.shared.set_wheel_mode(self.wheel_mode);
        self.shared.set_dir_lock_timeout_ms(self.dir_lock_timeout_ms);
        (self.on_config_change)(&self.current_config());
    }

    // --- Direct value entry ---

    fn start_edit(&mut self, kind: SliderKind) {
        // Only meaningful while direction lock is on; the chip is faded and
        // unclickable otherwise anyway (defensive).
        if self.wheel_mode != WheelMode::DirectionLock {
            return;
        }
        self.editing = Some(kind);
        self.focused_slider = Some(kind);
        // Start with an empty buffer so the user types a fresh value from
        // scratch, clearer than pre-filling the current value and making
        // them delete digits before typing.
        self.edit_buffer = String::new();
    }

    fn commit_edit(&mut self) {
        let Some(kind) = self.editing.take() else {
            return;
        };
        let buf = self.edit_buffer.trim();
        // Empty buffer = cancel: keep the current value unchanged.
        if buf.is_empty() {
            self.edit_buffer.clear();
            return;
        }
        let ok = match kind {
            SliderKind::Timeout => match buf.parse::<u32>() {
                Ok(v) => {
                    self.dir_lock_timeout_ms =
                        v.clamp(DIR_LOCK_TIMEOUT_MIN_MS, DIR_LOCK_TIMEOUT_MAX_MS);
                    true
                }
                Err(_) => false,
            },
        };
        self.edit_buffer.clear();
        if ok {
            self.save_and_sync();
        }
    }

    /// Human-readable value for a slider, used for the clickable label.
    fn value_display(&self, kind: SliderKind) -> String {
        match kind {
            SliderKind::Timeout => format!("{} ms", self.dir_lock_timeout_ms),
        }
    }

    // --- Keyboard nudging ---

    /// Sliders currently visible in the window, in visual order. Used for
    /// Tab/Shift-Tab focus cycling.
    fn visible_sliders(&self) -> Vec<SliderKind> {
        if self.wheel_mode == WheelMode::DirectionLock {
            vec![SliderKind::Timeout]
        } else {
            Vec::new()
        }
    }

    fn value_for(&self, kind: SliderKind) -> i32 {
        match kind {
            SliderKind::Timeout => self.dir_lock_timeout_ms as i32,
        }
    }

    fn set_value(&mut self, kind: SliderKind, value: i32) {
        match kind {
            SliderKind::Timeout => self.dir_lock_timeout_ms = value as u32,
        }
    }

    /// Move the focused slider by one nudge step in the given direction and
    /// persist the change. No-op while the timeout row is faded (mode off).
    fn nudge_focused(&mut self, direction: i32) {
        if self.wheel_mode != WheelMode::DirectionLock {
            return;
        }
        let Some(kind) = self.focused_slider else {
            return;
        };
        let (min, max) = kind.nudge_range();
        let next = (self.value_for(kind) + direction * kind.nudge_step()).clamp(min, max);
        self.set_value(kind, next);
        self.save_and_sync();
    }

    /// Step the timeout by one −/+ click (5 ms per press, clamped to the
    /// valid range) and persist. Delegates to [`nudge_focused`] after
    /// setting focus, so the math lives in one place. No-op while the
    /// timeout row is faded (mode off).
    fn step_timeout(&mut self, delta: i32) {
        self.focused_slider = Some(SliderKind::Timeout);
        self.nudge_focused(delta);
    }

    /// Move keyboard focus to the next/previous visible slider (Tab/Shift-Tab).
    fn cycle_focus(&mut self, backwards: bool) {
        let sliders = self.visible_sliders();
        if sliders.is_empty() {
            return;
        }
        let idx = self
            .focused_slider
            .and_then(|f| sliders.iter().position(|&s| s == f))
            .unwrap_or(0);
        let next = if backwards {
            (idx + sliders.len() - 1) % sliders.len()
        } else {
            (idx + 1) % sliders.len()
        };
        self.focused_slider = Some(sliders[next]);
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Read current values for display.
        let mode = self.wheel_mode;
        let enabled = mode == WheelMode::DirectionLock;
        let autostart = self.autostart;
        let hook_failed = self.hook_status == HookStatus::Failed;
        let theme = self.theme;

        div()
            .id("settings-root")
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            // Inter is embedded and registered at startup (see run_gui); it
            // renders cleaner at small sizes than the OS default.
            .font_family("Inter")
            .text_color(theme.text_primary)
            .text_sm()
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if this.editing.is_some() {
                    match event.keystroke.key.as_str() {
                        "enter" => {
                            this.commit_edit();
                            cx.notify();
                        }
                        "escape" => {
                            this.editing = None;
                            this.edit_buffer.clear();
                            cx.notify();
                        }
                        "backspace" => {
                            this.edit_buffer.pop();
                            cx.notify();
                        }
                        _ => {
                            if let Some(ch) = event.keystroke.key_char.as_deref() {
                                if ch.chars().all(|c| c.is_ascii_digit())
                                    && this.edit_buffer.len() + ch.len() <= 4
                                {
                                    this.edit_buffer.push_str(ch);
                                    cx.notify();
                                }
                            }
                        }
                    }
                } else {
                    match event.keystroke.key.as_str() {
                        // Nudge the focused slider (left/up = decrease,
                        // right/down = increase, both axes are natural
                        // for a value control).
                        "left" | "up" => {
                            this.nudge_focused(-1);
                            cx.notify();
                        }
                        "right" | "down" => {
                            this.nudge_focused(1);
                            cx.notify();
                        }
                        // Cycle focus between visible sliders.
                        "tab" => {
                            this.cycle_focus(event.keystroke.modifiers.shift);
                            cx.notify();
                        }
                        // Close on Escape
                        "escape" => {
                            this.shared.set_settings_rect(None);
                            cx.defer_in(window, |_this, window, _cx| {
                                window.remove_window();
                            });
                        }
                        _ => {}
                    }
                }
            }))
            // Content padding and gap are on the root div, no separate
            // card wrapper. Components float directly on the acrylic blur.
            .p(px(CONTENT_PADDING))
            .gap(px(SP_LG))
            // Warning banner when the mouse hook failed to install.
            .when(hook_failed, |this: gpui::Stateful<gpui::Div>| {
                this.child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(SP_MD))
                        .px(px(SP_LG))
                        .py(px(SP_MD))
                        .rounded(px(SP_MD))
                        .bg(theme.error)
                        .text_color(theme.text_on_accent)
                        .child(
                            svg()
                                .path("icons/warning.svg")
                                .w(px(18.))
                                .h(px(18.))
                                .text_color(theme.text_on_accent),
                        )
                        .child(
                            div()
                                .flex_1()
                                .child(
                                    "Mouse hook failed, wheel handling is disabled.",
                                ),
                        )
                        .child(
                            Button::new("retry-hook")
                                .label("Retry")
                                .on_click(cx.listener(
                                    |this, _event: &gpui::ClickEvent, _window, _cx| {
                                        this.shared.request_retry();
                                        crate::log::log(
                                            "Retry requested from the settings window",
                                        );
                                    },
                                )),
                        ),
                )
            })
            // Title row: app name on the left; two mode buttons on the right.
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_xl()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child("Sabitori"),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap(px(4.))
                            .child(
                                // Scroll Lock button: a 32x32 square icon button
                                // with the Tabler arrows-move-vertical icon.
                                // When active, the Direction Lock button fades.
                                {
                                    let lock_active = mode == WheelMode::Disable;
                                    let lock_faded = enabled; // Direction Lock is active
                                    let lock_theme = theme;
                                    div()
                                        .id("scroll-lock-toggle")
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .w(px(32.))
                                        .h(px(32.))
                                        .rounded(cx.theme().radius)
                                        .border_1()
                                        .border_color(if lock_active { lock_theme.accent } else { lock_theme.border })
                                        .bg(if lock_active { lock_theme.accent } else { lock_theme.control_bg })
                                        .text_color(if lock_active { lock_theme.text_on_accent } else { lock_theme.text_primary })
                                        .opacity(if lock_faded { 0.4 } else { 1.0 })
                                        .when(!lock_active, |this| {
                                            this.hover(|this| {
                                                this.bg(lock_theme.control_bg_hover)
                                                    .border_color(lock_theme.border)
                                                    .text_color(lock_theme.text_primary)
                                            })
                                        })
                                        .when(lock_active, |this| {
                                            let accent_hover = Hsla {
                                                h: lock_theme.accent.h,
                                                s: lock_theme.accent.s,
                                                l: (lock_theme.accent.l + 0.1).min(1.0),
                                                a: lock_theme.accent.a,
                                            };
                                            this.hover(|this| {
                                                this.bg(accent_hover)
                                                    .border_color(accent_hover)
                                                    .text_color(lock_theme.text_on_accent)
                                            })
                                        })
                                        .active(|this| {
                                            this.bg(if lock_active { lock_theme.accent } else { lock_theme.control_bg })
                                                .border_color(if lock_active { lock_theme.accent } else { lock_theme.border })
                                                .text_color(if lock_active { lock_theme.text_on_accent } else { lock_theme.text_primary })
                                        })
                                        .cursor_pointer()
                                        .on_click(cx.listener(|this, _event, _window, cx| {
                                            this.wheel_mode = if this.wheel_mode == WheelMode::Disable {
                                                WheelMode::Off
                                            } else {
                                                WheelMode::Disable
                                            };
                                            this.editing = None;
                                            this.edit_buffer.clear();
                                            this.save_and_sync();
                                            cx.notify();
                                        }))
                                        .child(
                                            gpui_component::Icon::default()
                                                .path("icons/arrows-move-vertical.svg")
                                                .size(px(20.))
                                                .text_color(if lock_active { lock_theme.text_on_accent } else { lock_theme.text_primary })
                                        )
                                }
                            )
                            .child(
                                // Direction Lock toggle: a 32x32 square icon button
                                // showing the Tabler `mouse` when direction lock is on
                                // and `mouse-off` when it's off. When Scroll Lock is
                                // active, this button fades.
                                div()
                                    .id("wheel-mode-toggle")
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .w(px(32.))
                                    .h(px(32.))
                                    .rounded(cx.theme().radius)
                                    .border_1()
                                    .border_color(if enabled { theme.accent } else { theme.border })
                                    .bg(if enabled { theme.accent } else { theme.control_bg })
                                    .text_color(if enabled { theme.text_on_accent } else { theme.text_primary })
                                    .opacity(if mode == WheelMode::Disable { 0.4 } else { 1.0 })
                                    .when(!enabled, |this| {
                                        this.hover(|this| {
                                            this.bg(theme.control_bg_hover)
                                                .border_color(theme.border)
                                                .text_color(theme.text_primary)
                                        })
                                    })
                                    .when(enabled, |this| {
                                        let accent_hover = Hsla {
                                            h: theme.accent.h,
                                            s: theme.accent.s,
                                            l: (theme.accent.l + 0.1).min(1.0),
                                            a: theme.accent.a,
                                        };
                                        this.hover(|this| {
                                            this.bg(accent_hover)
                                                .border_color(accent_hover)
                                                .text_color(theme.text_on_accent)
                                        })
                                    })
                                    .active(|this| {
                                        this.bg(if enabled { theme.accent } else { theme.control_bg })
                                            .border_color(if enabled { theme.accent } else { theme.border })
                                            .text_color(if enabled { theme.text_on_accent } else { theme.text_primary })
                                    })
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _event, _window, cx| {
                                        this.wheel_mode = if this.wheel_mode == WheelMode::DirectionLock {
                                            WheelMode::Off
                                        } else {
                                            WheelMode::DirectionLock
                                        };
                                        // The slider and value chip fade when off;
                                        // cancel any in-progress edit.
                                        this.editing = None;
                                        this.edit_buffer.clear();
                                        this.save_and_sync();
                                        cx.notify();
                                    }))
                                    .child(
                                        gpui_component::Icon::default()
                                            .path(if enabled { "icons/mouse.svg" } else { "icons/mouse-off.svg" })
                                            .size(px(24.))
                                            .text_color(if enabled { theme.text_on_accent } else { theme.text_primary })
                                    ),
                            ),
                    ),
            )
            // Scrolling timeout: always present, but faded and disabled when
            // the wheel-mode toggle is off. A centered label above a −/+ pair
            // flanking the clickable value chip (click the chip to type an
            // exact number; the buttons step in 5ms).
            .child(stepper_row(
                SliderKind::Timeout,
                self.value_display(SliderKind::Timeout),
                &StepperRowState {
                    is_editing: self.editing == Some(SliderKind::Timeout),
                    edit_buffer: self.edit_buffer.clone(),
                    enabled,
                },
                &theme,
                cx,
            ))
            // Divider separating the timeout feature from the rows below.
            .child(div().h(px(1.)).w_full().bg(theme.border))
            // Start with Windows: a centred gpui-component checkbox at the
            // bottom of the panel.
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_center()
                    .child(
                        Checkbox::new("autostart-checkbox")
                            .label("Start with Windows")
                            .checked(autostart)
                            .on_click(cx.listener(|this, checked, _window, cx| {
                                let enable = *checked;
                                this.autostart = enable;
                                if enable {
                                    if let Err(e) = crate::autostart::enable() {
                                        crate::log::log(&format!(
                                            "Failed to enable autostart: {e}"
                                        ));
                                        this.autostart = false;
                                    } else {
                                        crate::log::log("Autostart enabled");
                                    }
                                } else {
                                    if let Err(e) = crate::autostart::disable() {
                                        crate::log::log(&format!(
                                            "Failed to disable autostart: {e}"
                                        ));
                                        this.autostart = true;
                                    } else {
                                        crate::log::log("Autostart disabled");
                                    }
                                }
                                cx.notify();
                            })),
                    ),
            )
            // Open animation: a quick fade-in (ease-out) of the whole card
            // on mount, like a native tray flyout. Runs once per window open
            //, the animation state is per element id, so poll re-renders
            // and mode switches don't restart it.
            .with_animation(
                "flyout-in",
                Animation::new(Duration::from_millis(180)).with_easing(ease_out_quint()),
                |this, delta| this.opacity(delta),
            )
    }
}

/// Render-time state for the scrolling-timeout row, grouped to keep
/// `stepper_row`'s argument list under clippy's threshold.
struct StepperRowState {
    is_editing: bool,
    edit_buffer: String,
    /// False when wheel mode is off: the row is faded and inert.
    enabled: bool,
}

/// Build the scrolling-timeout row: a centered label above a segmented
/// ButtonGroup with three buttons, −, the clickable value chip, and +.
/// Clicking the value chip enters edit mode (type a fresh number, Enter
/// to commit). When `enabled` is false (wheel mode off) the whole row is
/// faded and unclickable.
fn stepper_row(
    kind: SliderKind,
    value_text: String,
    state: &StepperRowState,
    t: &Theme,
    cx: &mut Context<SettingsView>,
) -> gpui::Div {
    let value_display = if state.is_editing {
        if state.edit_buffer.is_empty() {
            // Show the current value as a faded placeholder so the user
            // knows what they're replacing, with a cursor to indicate
            // editing mode.
            SharedString::from(format!("{}|", value_text))
        } else {
            SharedString::from(format!("{}|", state.edit_buffer))
        }
    } else {
        SharedString::from(value_text)
    };
    // Owned copy so the closures can capture the palette without borrowing
    // the parameter reference.
    let t = *t;
    let editing = state.is_editing;
    let enabled = state.enabled;

    // The custom variant gives the buttons the panel's colors instead of
    // the gpui-component theme defaults.
    let variant = ButtonCustomVariant::new(cx)
        .color(t.control_bg)
        .foreground(t.text_primary)
        .border(t.border)
        .hover(t.control_bg_hover)
        .active(t.control_bg_hover);

    // − and + stepper buttons.
    let minus_btn = Button::new("step-minus")
        .custom(variant)
        .label("−")
        .w(px(28.))
        .h(px(28.))
        .px(px(0.))
        .text_color(t.text_primary)
        .rounded(px(6.))
        .disabled(!enabled)
        .on_click(cx.listener(move |this, _event: &gpui::ClickEvent, _window, cx| {
            this.step_timeout(-1);
            cx.notify();
        }));

    let plus_btn = Button::new("step-plus")
        .custom(variant)
        .label("+")
        .w(px(28.))
        .h(px(28.))
        .px(px(0.))
        .text_color(t.text_primary)
        .rounded(px(6.))
        .disabled(!enabled)
        .on_click(cx.listener(move |this, _event: &gpui::ClickEvent, _window, cx| {
            this.step_timeout(1);
            cx.notify();
        }));

    // Middle button: the value chip. When editing, `.selected(true)`
    // paints it with the accent color so it reads as active. Clicking it
    // enters edit mode (only when enabled).
    let value_btn = Button::new("value-timeout")
        .custom(variant)
        .label(value_display)
        .min_w(px(80.))
        .h(px(28.))
        .px(px(SP_LG))
        .text_color(if editing {
            t.text_on_accent
        } else {
            t.text_secondary
        })
        .selected(editing)
        .rounded(px(6.))
        .disabled(!enabled)
        .when(enabled, |this: Button| {
            this.on_click(cx.listener(move |this, _event: &gpui::ClickEvent, _window, cx| {
                this.start_edit(kind);
                cx.notify();
            }))
        });

    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(SP_MD))
        // Faded while wheel mode is off, so the disabled state reads
        // visually instead of the row just vanishing.
        .when(!enabled, |this: gpui::Div| this.opacity(0.4))
        .child(div().text_color(t.text_secondary).child(kind.label().into_element()))
        .child(
            ButtonGroup::new("timeout-group")
                .compact()
                .child(minus_btn)
                .child(value_btn)
                .child(plus_btn),
        )
}

/// A tiny, never-shown window that keeps the application alive.
///
/// GPUI's Windows platform quits the whole app when the *last* window is
/// closed (`close_one_window` posts `WM_QUIT`). Without this anchor, closing
/// the settings window, the app's only window, would kill the tray app
/// every time. The anchor is created hidden at startup and deliberately
/// leaked, so the settings window is never the last one and closing it just
/// closes that window.
struct AnchorView;

impl Render for AnchorView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

/// Create the hidden anchor window that keeps the tray app alive after the
/// settings window closes (see [`AnchorView`]). Called once at startup.
pub fn open_anchor_window(cx: &mut App) -> Result<(), anyhow::Error> {
    let options = WindowOptions {
        // Far off-screen and 1x1, and never shown, it is not visible
        // anywhere (no taskbar entry, no Alt-Tab) and can never be closed.
        window_bounds: Some(gpui::WindowBounds::Windowed(gpui::Bounds::new(
            gpui::point(px(-10000.), px(-10000.)),
            gpui::size(px(1.), px(1.)),
        ))),
        show: false,
        kind: gpui::WindowKind::Normal,
        is_resizable: false,
        is_minimizable: false,
        focus: false,
        ..Default::default()
    };
    let handle = cx.open_window(options, |_window, cx| cx.new(|_| AnchorView))?;
    // WindowHandle is Copy, so dropping it does nothing, GPUI keeps the
    // window alive in its internal window map for the lifetime of the app.
    // The window is never shown and can't be closed by the user, so it
    // is a permanent anchor that prevents the app from quitting when
    // the settings window closes.
    let _ = handle;
    Ok(())
}

/// Open the settings window as a native tray flyout: a borderless tool
/// window (no taskbar button, no Alt-Tab entry, no min/max buttons) placed
/// just above the tray icon, or, when the icon is hidden in the overflow
/// area, just above the taskbar's right edge, like the volume/network
/// flyouts. Clicking anywhere outside dismisses it (focus loss).
///
/// `tray_rect` is the tray icon's current on-screen rectangle, used for
/// positioning; pass `None` to fall back to the work area's bottom-right
/// corner.
pub fn open_settings_window(
    cx: &mut App,
    shared: Arc<SharedState>,
    on_config_change: impl Fn(&Config) + 'static,
    tray_rect: Option<tray_icon::Rect>,
) -> Result<WindowHandle<SettingsView>, anyhow::Error> {
    // The window hugs its content: the height comes from the rows it will
    // actually show (wheel mode, hook failure banner).
    let size = settings_window_size(shared.hook_status() == HookStatus::Failed);
    let origin = flyout_origin(size, tray_rect.map(IconRect::from), primary_work_area(), system_scale());

    let options = WindowOptions {
        window_bounds: Some(gpui::WindowBounds::Windowed(gpui::Bounds::new(origin, size))),
        // PopUp maps to WS_EX_TOOLWINDOW with no caption on Windows: no
        // taskbar button, no Alt-Tab entry, no min/max buttons, borderless:
        // the look of a native tray flyout. Transparent so the root's rounded
        // corners show the desktop instead of a black wedge.
        kind: gpui::WindowKind::PopUp,
        titlebar: None,
        window_background: gpui::WindowBackgroundAppearance::Blurred,
        focus: true,
        show: true,
        // `is_movable: false` is what disables the resize cursor on this
        // fixed window: gpui's WM_NCHITTEST handler only reports resize edges
        // (HTTOP/HTBOTTOM/...) when is_movable is set, and the flyout is
        // positioned programmatically anyway.
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        ..Default::default()
    };

    let shared_state = shared.clone();
    let handle = cx.open_window(options, |_window, cx| {
        cx.new(|cx| SettingsView::new(shared, Box::new(on_config_change), cx))
    })?;

    // Keyboard events only reach the focused element in this GPUI version, so
    // focus the root element on open to enable typing, arrow nudging, and
    // Escape-to-close. Also register the native flyout behavior: losing
    // focus (clicking anywhere outside, Alt-Tab, opening the tray menu)
    // dismisses the window. The subscription is kept in the view so the
    // listener lives exactly as long as the window does.
    handle.update(cx, |view, window, cx| {
        view.focus_handle.focus(window);
        let listener_shared = shared_state.clone();
        // The window is activated programmatically, which Windows can deny
        // (SetForegroundWindow restrictions), so the focus churn around a
        // tray-click open can fire on_focus_out a moment after opening and
        // kill the flyout before the user sees it. Ignore focus-out during a
        // short grace period after open, a deliberate outside click always
        // comes later than this.
        let opened_at = Instant::now();
        view.focus_out_sub = Some(window.on_focus_out(
            &view.focus_handle,
            cx,
            move |_event, window, _cx| {
                if opened_at.elapsed() < OPEN_FOCUS_GRACE {
                    return;
                }
                listener_shared.set_settings_rect(None);
                window.remove_window();
            },
        ));

        // A tray flyout must draw above foreground and fullscreen windows.
        // GPUI's PopUp maps to a plain tool window without WS_EX_TOPMOST, so
        // pin the native window topmost through its raw handle, and publish
        // its on-screen rect so the hook can dismiss it on outside clicks.
        // Also strip any caption/border/size styles gpui's window class or
        // Windows added, so the flyout is a clean, non-resizable client-area
        // rectangle (no dialog frame, no resize border, it's a tray window).
        if let Ok(native) = window.window_handle() {
            if let RawWindowHandle::Win32(win32) = native.as_raw() {
                let hwnd = HWND(win32.hwnd.get() as *mut _);
                // Remember the native handle and the starting client height so
                // the refresh poll can resize the window to its content.
                view.hwnd = hwnd.0 as isize;
                view.client_h = size.height.into();
                unsafe {
                    let style = WINDOW_STYLE(GetWindowLongPtrW(hwnd, GWL_STYLE) as u32);
                    let stripped = style
                        & !(WS_CAPTION | WS_THICKFRAME | WS_SYSMENU | WS_MAXIMIZEBOX
                            | WS_MINIMIZEBOX);
                    if stripped != style {
                        let _ = SetWindowLongPtrW(hwnd, GWL_STYLE, stripped.0 as isize);
                        let _ = SetWindowPos(
                            hwnd,
                            None,
                            0,
                            0,
                            0,
                            0,
                            SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER
                                | SWP_NOACTIVATE,
                        );
                    }
                    // Also strip any extended border styles (client edge,
                    // window edge) that add a non-client gap between the
                    // window rect and the client area where the card renders.
                    let ex_style = WINDOW_EX_STYLE(GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32);
                    let ex_stripped = ex_style & !(WS_EX_CLIENTEDGE | WS_EX_WINDOWEDGE);
                    if ex_stripped != ex_style {
                        let _ = SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex_stripped.0 as isize);
                    }
                    // After stripping styles, resize the window so the
                    // client area exactly matches the intended content
                    // size. GPUI created the window with border offsets
                    // baked in, now that borders are gone, the window is
                    // too large, leaving acrylic visible around the card.
                    let mut rc = RECT::default();
                    if GetWindowRect(hwnd, &mut rc).is_ok() {
                        let mut client = RECT::default();
                        let _ = GetClientRect(hwnd, &mut client);
                        let dx = (rc.right - rc.left) - (client.right - client.left);
                        let dy = (rc.bottom - rc.top) - (client.bottom - client.top);
                        if dx > 0 || dy > 0 {
                            // Shrink the window by the non-client delta,
                            // keeping the bottom-right corner fixed (the
                            // flyout grows upward from the tray).
                            let new_w = (rc.right - rc.left) - dx;
                            let new_h = (rc.bottom - rc.top) - dy;
                            let new_left = rc.left + (dx / 2);
                            let new_top = rc.top + (dy / 2) - dy; // shift up by bottom border
                            let _ = SetWindowPos(
                                hwnd,
                                None,
                                new_left,
                                new_top,
                                new_w,
                                new_h,
                                SWP_FRAMECHANGED | SWP_NOZORDER | SWP_NOACTIVATE,
                            );
                        }
                    }
                    let _ = SetWindowPos(
                        hwnd,
                        Some(HWND_TOPMOST),
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                    );

                    // Override the acrylic blur tint with a darker color.
                    // GPUI's Blurred mode sets acrylic with a near-transparent
                    // tint (0,0,0,0). We re-apply it with a dark, semi-opaque
                    // tint so the window has a dark frosted glass backing
                    // without needing a card background fill.
                    let want_dark = !crate::theme::is_light_mode();
                    set_acrylic_tint(hwnd, want_dark);
                    let dark = if !crate::theme::is_light_mode() {
                        1i32
                    } else {
                        0i32
                    };
                    let _ = DwmSetWindowAttribute(
                        hwnd,
                        DWMWA_USE_IMMERSIVE_DARK_MODE,
                        &dark as *const _ as *const _,
                        std::mem::size_of::<i32>() as u32,
                    );
                    // Round the window corners at the OS level so the
                    // acrylic blur is clipped to the same rounded shape.
                    let corner = DWMWCP_ROUND.0;
                    let _ = DwmSetWindowAttribute(
                        hwnd,
                        DWMWA_WINDOW_CORNER_PREFERENCE,
                        &corner as *const _ as *const _,
                        std::mem::size_of::<i32>() as u32,
                    );
                    // Windows 11 draws a thin border around rounded windows by
                    // default, which makes the flyout look like a card sitting
                    // inside a window. Suppress it so the surface is flush.
                    let border_color = DWMWA_COLOR_NONE;
                    let _ = DwmSetWindowAttribute(
                        hwnd,
                        DWMWA_BORDER_COLOR,
                        &border_color as *const _ as *const _,
                        std::mem::size_of::<u32>() as u32,
                    );

                    let mut rc = RECT::default();
                    if GetWindowRect(hwnd, &mut rc).is_ok() {
                        shared_state
                            .set_settings_rect(Some((rc.left, rc.top, rc.right, rc.bottom)));
                    }
                }
            }
        }
    })?;

    Ok(handle)
}

/// Launch the GPUI application with a callback for the launch event. The
/// embedded icon assets are registered so `svg()` elements can resolve
/// `icons/*.svg` paths, and the bundled Inter weights are registered so
/// `font_family("Inter")` resolves on any machine.
pub fn run_gui(on_launch: impl Fn(&mut App) + 'static) {
    Application::new().with_assets(crate::icons::IconAssets).run(move |cx| {
        let _ = cx.text_system().add_fonts(vec![
            Cow::Borrowed(INTER_REGULAR),
            Cow::Borrowed(INTER_MEDIUM),
        ]);
        on_launch(cx);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flyout_origin_centers_on_icon() {
        let size = gpui::size(px(400.), px(300.));
        let icon = IconRect { left: 900.0, top: 1000.0, width: 32 };
        let work = WorkArea { left: 0, top: 0, right: 1920, bottom: 1040 };
        let origin = flyout_origin(size, Some(icon), work, 1.0);
        // Centered on icon: icon center = 900 + 16 = 916, minus half width = 716.
        assert!((f32::from(origin.x) - 716.0).abs() < 1.0);
        // Above icon: icon top = 1000, minus height - gap = 1000 - 300 - 8 = 692.
        assert!((f32::from(origin.y) - 692.0).abs() < 1.0);
    }

    #[test]
    fn flyout_origin_fallback_bottom_right() {
        let size = gpui::size(px(400.), px(300.));
        let work = WorkArea { left: 0, top: 0, right: 1920, bottom: 1040 };
        let origin = flyout_origin(size, None, work, 1.0);
        // Bottom-right: 1920 - 400 - 8 = 1512, 1040 - 300 - 8 = 732.
        assert!((f32::from(origin.x) - 1512.0).abs() < 1.0);
        assert!((f32::from(origin.y) - 732.0).abs() < 1.0);
    }

    #[test]
    fn flyout_origin_clamps_to_work_area() {
        // Icon far left, window should clamp to work area left edge.
        let size = gpui::size(px(400.), px(300.));
        let icon = IconRect { left: 0.0, top: 1000.0, width: 32 };
        let work = WorkArea { left: 0, top: 0, right: 1920, bottom: 1040 };
        let origin = flyout_origin(size, Some(icon), work, 1.0);
        // icon center = 16, minus 200 = -184, clamped to 0.
        assert!((f32::from(origin.x) - 0.0).abs() < 1.0);
    }

    #[test]
    fn flyout_origin_dpi_scaling() {
        // At 150% DPI, physical pixels are divided by 1.5.
        let size = gpui::size(px(400.), px(300.));
        let icon = IconRect { left: 1500.0, top: 1500.0, width: 48 };
        let work = WorkArea { left: 0, top: 0, right: 2880, bottom: 1560 };
        let origin = flyout_origin(size, Some(icon), work, 1.5);
        // icon center = (1500 + 24) / 1.5 = 1016, minus 200 = 816.
        assert!((f32::from(origin.x) - 816.0).abs() < 1.0);
    }

    #[test]
    fn settings_window_size_width_constant() {
        let s = settings_window_size(false);
        // Narrow minimal panel: 280px wide.
        assert_eq!(f32::from(s.width), 280.0);
    }

    #[test]
    fn settings_window_size_banner_adds_height() {
        let no_banner = settings_window_size(false);
        let with_banner = settings_window_size(true);
        assert!(f32::from(with_banner.height) > f32::from(no_banner.height));
    }

    #[test]
    fn flyout_origin_window_taller_than_work_area() {
        // If the window is taller than the work area, it should clamp to
        // the top edge (not go negative or overflow).
        let size = gpui::size(px(400.), px(2000.));
        let work = WorkArea { left: 0, top: 0, right: 1920, bottom: 1040 };
        let origin = flyout_origin(size, None, work, 1.0);
        // y clamps to top (0) since bottom - h = 1040 - 2000 = -960 < 0.
        assert_eq!(f32::from(origin.y), 0.0);
    }

    #[test]
    fn flyout_origin_window_wider_than_work_area() {
        // If the window is wider than the work area, it should clamp to
        // the left edge.
        let size = gpui::size(px(3000.), px(300.));
        let work = WorkArea { left: 0, top: 0, right: 1920, bottom: 1040 };
        let origin = flyout_origin(size, None, work, 1.0);
        assert_eq!(f32::from(origin.x), 0.0);
    }

    #[test]
    fn flyout_origin_icon_near_right_edge_clamps() {
        // Icon near the right edge of the work area: the window should
        // not overflow past the right edge.
        let size = gpui::size(px(400.), px(300.));
        let icon = IconRect { left: 1850.0, top: 1000.0, width: 32 };
        let work = WorkArea { left: 0, top: 0, right: 1920, bottom: 1040 };
        let origin = flyout_origin(size, Some(icon), work, 1.0);
        // icon center = 1866, minus 200 = 1666, but right edge is 1920,
        // so clamped to 1920 - 400 = 1520.
        assert!((f32::from(origin.x) - 1520.0).abs() < 1.0);
    }

    #[test]
    fn settings_window_size_exact_height_no_banner() {
        // No banner: rows = [TITLE, TIMEOUT, DIVIDER, AUTOSTART] = 4 rows.
        // height = PAD(32) + GAP(12)*3 + sum(36+56+1+26) + 2*PANEL_MARGIN(0)
        //        = 32 + 36 + 119 + 0 = 187
        let s = settings_window_size(false);
        assert_eq!(f32::from(s.height), 187.0);
    }

    #[test]
    fn settings_window_size_exact_height_with_banner() {
        // With banner: rows = [BANNER, TITLE, TIMEOUT, DIVIDER, AUTOSTART]
        // = 5 rows.
        // height = PAD(32) + GAP(12)*4 + sum(44+36+56+1+26)
        //        + 2*PANEL_MARGIN(0)
        //        = 32 + 48 + 163 + 0 = 243
        let s = settings_window_size(true);
        assert_eq!(f32::from(s.height), 243.0);
    }


}
