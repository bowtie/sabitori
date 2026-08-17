//! Autostart on login: manage a registry Run key so Sabitori launches
//! automatically when the user logs in.
//!
//! The key is `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\Sabitori`.
//! Setting it to the current executable path makes Windows start the app
//! at logon; deleting it removes the autostart. This is the standard
//! per-user autostart mechanism, no admin rights needed.

use windows::core::PCWSTR;
use windows::Win32::System::Registry::{
    RegCloseKey, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, RegDeleteValueW, HKEY,
    HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_SAM_FLAGS, REG_SZ,
};
use windows::Win32::Foundation::ERROR_SUCCESS;

use crate::wide::to_wide;

/// Registry subkey for the Windows Run list (per-user).
const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
/// Value name under the Run key.
const VALUE_NAME: &str = "Sabitori";

/// Open the Run registry key with the given access rights.
unsafe fn open_run_key(access: REG_SAM_FLAGS) -> Result<HKEY, String> {
    let subkey = to_wide(RUN_KEY);
    let mut handle = HKEY::default();
    let result = RegOpenKeyExW(
        HKEY_CURRENT_USER,
        PCWSTR(subkey.as_ptr()),
        None,
        access,
        &mut handle,
    );
    if result == ERROR_SUCCESS {
        Ok(handle)
    } else {
        Err(format!("RegOpenKeyExW failed: error {result:?}"))
    }
}

/// Check whether autostart is currently enabled (the Run value exists and
/// points to the current executable).
pub fn is_enabled() -> bool {
    unsafe {
        let Ok(handle) = open_run_key(KEY_READ) else {
            return false;
        };
        let name = to_wide(VALUE_NAME);
        let mut buf = [0u16; 1024];
        let mut buf_len = (buf.len() * 2) as u32;
        let result = RegQueryValueExW(
            handle,
            PCWSTR(name.as_ptr()),
            None,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut buf_len),
        );
        let _ = RegCloseKey(handle);

        if result != ERROR_SUCCESS {
            return false;
        }

        // Compare the stored path with the current exe path (quoted form).
        let len = (buf_len / 2) as usize;
        let end = buf[..len].iter().position(|&c| c == 0).unwrap_or(len);
        let stored = String::from_utf16_lossy(&buf[..end]);
        let exe = std::env::current_exe()
            .map(|p| format!("\"{}\"", p.to_string_lossy()))
            .unwrap_or_default();
        stored.eq_ignore_ascii_case(&exe)
    }
}

/// Enable autostart: set the Run value to the current executable path.
pub fn enable() -> Result<(), String> {
    unsafe {
        let handle = open_run_key(KEY_WRITE)?;
        let name = to_wide(VALUE_NAME);
        let exe = std::env::current_exe()
            .map_err(|e| format!("current_exe failed: {e}"))?;
        // Wrap the path in quotes so Windows doesn't split it at spaces
        // when launching from the Run key (e.g. "C:\Program Files\...").
        let quoted = format!("\"{}\"", exe.to_string_lossy());
        let path = to_wide(&quoted);
        let path_bytes: &[u8] = std::slice::from_raw_parts(
            path.as_ptr() as *const u8,
            path.len() * 2,
        );

        let result = RegSetValueExW(
            handle,
            PCWSTR(name.as_ptr()),
            None,
            REG_SZ,
            Some(path_bytes),
        );
        let _ = RegCloseKey(handle);

        if result == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(format!("RegSetValueExW failed: error {result:?}"))
        }
    }
}

/// Disable autostart: delete the Run value. If the value is already absent
/// (e.g. the user removed it manually), that is success: the desired state
/// (no autostart) is already achieved.
pub fn disable() -> Result<(), String> {
    unsafe {
        let handle = open_run_key(KEY_WRITE)?;
        let name = to_wide(VALUE_NAME);

        let result = RegDeleteValueW(handle, PCWSTR(name.as_ptr()));
        let _ = RegCloseKey(handle);

        // ERROR_FILE_NOT_FOUND (2) means the value doesn't exist, already
        // disabled, so treat it as success.
        if result == ERROR_SUCCESS || result == windows::Win32::Foundation::ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            Err(format!("RegDeleteValueW failed: error {result:?}"))
        }
    }
}
