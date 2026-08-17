// Build as a Windows GUI application so no console window is allocated
// when the app is launched (it lives in the system tray).
#![windows_subsystem = "windows"]

mod autostart;
mod config;
mod gui;
mod hook;
mod icons;
mod log;
mod notify;
mod single_instance;
mod theme;
mod tray;
mod wide;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use config::{Config, WheelMode};
use hook::{HookStatus, SharedState};
use tray::TrayAction;

/// Tooltip text that reflects the hook installation status, so a failed
/// install is visible without opening the settings window.
fn hook_tooltip(status: HookStatus) -> String {
    match status {
        HookStatus::Pending => "Sabitori (starting…)".to_string(),
        HookStatus::Installed => "Sabitori".to_string(),
        HookStatus::Failed => "Sabitori (mouse hook failed)".to_string(),
    }
}


/// Spawn a thread that installs the low-level mouse hook and pumps its Win32
/// message loop. Records the thread's OS id in `tid_holder` so the app can
/// post `WM_QUIT` to it on shutdown.
fn spawn_hook_thread(
    shared: Arc<SharedState>,
    tid_holder: Arc<Mutex<Option<u32>>>,
    finished: Arc<AtomicBool>,
) {
    // The exactly-one-hook-thread invariant: never start a second hook thread
    // while one is alive, it would install its own WH_MOUSE_LL hook and
    // double every scroll. This is a fast-path refusal; the thread's entry
    // re-claims the slot authoritatively, so a lost race is caught there
    // instead of here.
    if hook::hook_thread_active() {
        log::log("Refusing to spawn hook thread: a hook thread is already active");
        return;
    }

    thread::spawn(move || {
        // Claim the hook-thread slot at the very start of entry. If another
        // thread won the race, stand down without installing anything.
        if !hook::try_claim_hook_thread() {
            log::log("Hook thread standing down: another hook thread is already active");
            finished.store(true, Ordering::Relaxed);
            return;
        }

        if let Ok(mut tid) = tid_holder.lock() {
            *tid = Some(hook::current_thread_id());
        }

        let hook = match hook::install_hook(shared.clone()) {
            Ok(h) => {
                shared.set_hook_status(HookStatus::Installed);
                log::log("Mouse hook installed");
                h
            }
            Err(e) => {
                // Release the slot *before* signalling Failed to the GUI: the
                // retry flow only respawns once it observes Failed, so
                // clearing first guarantees a retry's claim can never collide
                // with this thread's release.
                hook::release_hook_thread();
                shared.set_hook_status(HookStatus::Failed);
                log::log(&format!("Failed to install hook: {e}"));
                // Publish `finished` *before* the blocking error dialog: a
                // retry that respawns the thread while the dialog is still up
                // resets this flag for the replacement thread, and this
                // thread's late store must not clobber it. The dialog is
                // purely informational at this point, the thread's cleanup
                // is done.
                finished.store(true, Ordering::Relaxed);
                notify::error(
                    "Sabitori",
                    &format!("Failed to install the mouse hook:\n{e}"),
                );
                return;
            }
        };

        // Run the message loop, required for WH_MOUSE_LL.
        hook::run_message_loop();

        // Clean up on exit. The slot is released last: after the hook is
        // removed no further callbacks can fire, so the flag accurately
        // describes thread aliveness for the entire exit path.
        hook::remove_hook(hook);
        hook::release_hook_thread();
        log::log("Hook thread stopped");
        finished.store(true, Ordering::Relaxed);
    });
}

fn main() {
    // Single-instance guard: a second launch logs and exits before installing
    // a hook, creating a tray icon, or opening any window.
    if !single_instance::acquire() {
        // Ask the running instance to bring its settings window forward
        // (best-effort; the guard itself does not depend on it).
        single_instance::signal_second_launch();
        // The rejection line is buffered; flush it before this short-lived
        // process exits so it actually reaches the log.
        log::flush();
        return;
    }

    // Load config from disk (or defaults).
    let config = config::load();

    // Create shared state with initial config values.
    let shared = SharedState::new(
        config.wheel_mode,
        config.direction_lock_timeout_ms,
    );

    // Track the current hook thread's OS id so the app can post WM_QUIT to it
    // on shutdown. Shared with the tray poll task, which may spawn replacement
    // hook threads after a failed install.
    let hook_tid: Arc<Mutex<Option<u32>>> = Arc::new(Mutex::new(None));
    // Set once a hook thread has finished its cleanup (or failed to start),
    // so shutdown can wait for the hook thread instead of racing it.
    let hook_finished: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));

    // Spawn the initial hook thread with its own Win32 message pump.
    spawn_hook_thread(shared.clone(), hook_tid.clone(), hook_finished.clone());

    // Launch GPUI on the main thread. `shutdown_*` stays behind for the
    // post-run shutdown sequence below; `poll_*` goes into the GUI closure
    // for the poll task, where the settings window's Retry button respawns
    // the hook thread with the same tid/finished tracking.
    let gui_shared = shared.clone();
    let shutdown_hook_tid = hook_tid.clone();
    let shutdown_hook_finished = hook_finished.clone();
    let poll_hook_tid = hook_tid.clone();
    let poll_hook_finished = hook_finished.clone();
    gui::run_gui(move |cx| {
        // gpui-component components (the Start-with-Windows checkbox, the
        // wheel-mode Toggle, the stepper buttons) read their palette from a
        // global Theme. Initialize from the embedded base theme, then inject
        // the live Windows system accent color so the components match the
        // user's personalization settings.
        let theme_set: gpui_component::ThemeSet = serde_json::from_str(
            include_str!("../assets/theme.json"),
        )
        .expect("embedded assets/theme.json must parse as a ThemeSet");
        let mut component_theme = gpui_component::Theme::default();
        for config in &theme_set.themes {
            component_theme.apply_config(&std::rc::Rc::new(config.clone()));
        }
        let mode = if crate::theme::is_light_mode() {
            gpui_component::ThemeMode::Light
        } else {
            gpui_component::ThemeMode::Dark
        };
        cx.set_global(component_theme);
        gpui_component::Theme::change(mode, None, cx);

        // Inject the Windows system accent color into the global theme so
        // the Checkbox, Toggle, and buttons use it for their active states.
        {
            let [r, g, b] = crate::theme::accent_color();
            let accent = gpui::Rgba {
                r: r as f32 / 255.0,
                g: g as f32 / 255.0,
                b: b as f32 / 255.0,
                a: 1.0,
            }
            .into();
            let white = gpui::Hsla {
                h: 0.0,
                s: 0.0,
                l: 1.0,
                a: 1.0,
            };
            let t = gpui_component::Theme::global_mut(cx);
            t.primary = accent;
            t.accent = accent;
            t.primary_hover = accent;
            t.accent_foreground = white;
            t.primary_foreground = white;
        }

        // Set up the tray icon.
        let tray = match tray::Tray::new(&gui_shared) {
            Ok(t) => t,
            Err(e) => {
                log::log(&format!("Failed to create tray icon: {e}"));
                notify::error(
                    "Sabitori",
                    &format!("Failed to create the tray icon:\n{e}"),
                );
                // Without a tray icon there is no way for the user to quit,
                // so shut down instead of leaving an invisible process running.
                cx.quit();
                return;
            }
        };

        // Clone the checkable menu items and the icon handle so the poll task
        // can keep their state in sync. The tray itself is leaked to keep it
        // alive for the lifetime of the app.
        // The settings window must never be the app's last window: GPUI's
        // Windows platform quits when the last window closes, which would
        // kill the tray app every time the settings window is closed. A
        // hidden anchor window created at startup keeps the app alive.
        if let Err(e) = gui::open_anchor_window(cx) {
            log::log(&format!("Failed to create anchor window: {e}"));
        }

        let tray_icon = tray.icon();
        std::mem::forget(tray);

        // Poll for tray menu events using an async task.
        // Use smol::Timer (re-exported by GPUI) for non-blocking sleep.
        let poll_shared = gui_shared.clone();
        // Fresh locals so the async block can capture them by move without
        // consuming this Fn closure's environment (Fn closures may be called
        // more than once, so their captures can't be moved out).
        let poll_tid = poll_hook_tid.clone();
        let poll_finished = poll_hook_finished.clone();
        cx.spawn(async move |cx| {
            let mut last_tip_key: Option<(HookStatus, WheelMode)> = None;
            // The settings window handle, so tray clicks / menu items reuse an
            // already-open window instead of stacking new ones. `update`
            // fails once the window is closed, which is our aliveness check.
            let mut settings_window: Option<gpui::WindowHandle<gui::SettingsView>> = None;
            loop {
                // Keep the tray tooltip and icon in sync with the hook status
                // and wheel mode. The icon color changes with the mode so the
                // user can see the active mode at a glance.
                let hook_status = poll_shared.hook_status();
                let wheel_mode = poll_shared.wheel_mode();
                let tip_key = (hook_status, wheel_mode);
                if last_tip_key != Some(tip_key) {
                    last_tip_key = Some(tip_key);
                    let _ = tray_icon.set_tooltip(Some(hook_tooltip(hook_status)));
                    let _ = tray_icon.set_icon(Some(tray::icon_for_mode(wheel_mode)));
                }

                // A failed hook no longer quits the app: it stays running so
                // the settings window can show the failure banner with a
                // Retry button. The button's request is consumed here and the
                // hook thread is respawned. The failed thread released the
                // hook-thread slot before signalling Failed (and published
                // `finished`), so the spawn can't collide with it and the
                // reset below can't be clobbered by its late store.
                if poll_shared.take_retry_requested() {
                    log::log("Retrying mouse hook install");
                    poll_finished.store(false, Ordering::Relaxed);
                    spawn_hook_thread(poll_shared.clone(), poll_tid.clone(), poll_finished.clone());
                }

                // Native flyout dismissal: the hook thread flagged a mouse
                // button-down outside the open settings window. Close it.
                // Works over fullscreen apps, where the activation-based
                // focus-out dismiss can't fire. `dismissed_now` also stops the
                // same gesture's tray-click (the button-up that follows the
                // outside button-down) from reopening the window, a
                // time-based window would instead eat legitimate reopen
                // clicks that land shortly after a dismissal.
                let dismissed_now = poll_shared.take_dismiss_requested();
                if dismissed_now {
                    if let Some(handle) = settings_window.take() {
                        poll_shared.set_settings_rect(None);
                        let _ = handle.update(cx, |_, window, _| window.remove_window());
                    }
                }

                // Check for tray menu events, tray icon clicks, and
                // second-launch signals (another instance started). A
                // tray-icon left-click *toggles* the settings flyout (open if
                // closed, close if open), like the native volume/network
                // flyouts; the menu Settings item and a second launch always
                // open or bring it forward.
                let tray_click = tray::poll_tray_click();
                if let Some(action) = tray::poll_menu_event()
                    .or(tray_click)
                    .or_else(|| {
                        single_instance::poll_second_launch()
                            .then_some(TrayAction::Settings)
                    })
                {
                    match action {
                        TrayAction::Settings => {
                            let shared_for_window = poll_shared.clone();
                            cx.update(|cx| {
                                // A tray-icon click on an open window toggles
                                // it closed. (The menu item and second launch
                                // skip this, they always open/bring-forward.)
                                let toggled_closed = tray_click.is_some()
                                    && settings_window.as_ref().and_then(|handle| {
                                        // Probe once to force the purge of a
                                        // window already closed (e.g. by the
                                        // focus-out dismiss this same click
                                        // triggered): `update` on a closed
                                        // window succeeds (it purges it), so
                                        // the second probe reflects reality.
                                        let _ = handle.update(cx, |_, _, _| {});
                                        handle
                                            .update(cx, |_, window, _| window.remove_window())
                                            .ok()
                                    })
                                    .is_some();
                                if toggled_closed {
                                    settings_window = None;
                                    poll_shared.set_settings_rect(None);
                                    return;
                                }
                                // A tray click whose focus-out dismiss already
                                // ran before this poll tick must stay closed:
                                // reopening here would make the toggle
                                // flicker back open.
                                if tray_click.is_some() && settings_window.is_none() && dismissed_now
                                {
                                    return;
                                }

                                // Reuse an already-open settings window
                                // (e.g. from a tray double-click or repeated
                                // M1 clicks) instead of stacking new ones.
                                let reused = settings_window.as_ref().and_then(|handle| {
                                    // Probe once to force the purge of a
                                    // window that was closed but not yet
                                    // removed from the app's window map:
                                    // `update` on such a window succeeds (it
                                    // purges it), so a single probe would
                                    // wrongly report it alive and swallow
                                    // the click. The second probe then
                                    // reflects reality.
                                    let _ = handle.update(cx, |_, _, _| {});
                                    handle
                                        .update(cx, |view, window, _| {
                                            view.focus_window(window);
                                        })
                                        .ok()
                                });
                                if reused.is_some() {
                                    // Bring the existing window forward.
                                    cx.activate(true);
                                } else {
                                    let on_change = move |cfg: &Config| {
                                        if let Err(e) = config::save(cfg) {
                                            log::log(&format!("Failed to save config: {e}"));
                                        }
                                    };
                                    // The icon's screen rectangle anchors the
                                    // flyout position (falls back to the
                                    // taskbar edge when hidden in overflow).
                                    let tray_rect = tray_icon.rect();
                                    match gui::open_settings_window(
                                        cx,
                                        shared_for_window,
                                        on_change,
                                        tray_rect,
                                    ) {
                                        Ok(handle) => settings_window = Some(handle),
                                        Err(e) => log::log(&format!(
                                            "Failed to open settings window: {e}"
                                        )),
                                    }
                                }
                            })
                            .ok();
                        }
                        TrayAction::Quit => {
                            cx.update(|cx| {
                                cx.quit();
                            })
                            .ok();
                            break;
                        }
                    }
                }

                // Non-blocking async sleep, yields to GPUI's executor.
                gpui::Timer::after(Duration::from_millis(50)).await;
            }
        })
        .detach();
    });

    // Ask the current hook thread's message loop to exit. Re-read the id on
    // each attempt in case a retry replaced the thread.
    for _ in 0..50 {
        let tid = shutdown_hook_tid.lock().ok().and_then(|guard| *guard);
        match tid {
            Some(t) if hook::post_quit(t) => break,
            _ => thread::sleep(Duration::from_millis(10)),
        }
    }

    // Give the hook thread a moment to finish its cleanup (unhook + final log)
    // so the shutdown sequence is fully written to the log. Bounded so a
    // wedged thread can never hang the exit.
    let deadline = Instant::now() + Duration::from_millis(1500);
    while !shutdown_hook_finished.load(Ordering::Relaxed) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }

    log::log("Sabitori exiting");
    // Push any remaining buffered lines to disk at the clean shutdown point.
    log::flush();
}
