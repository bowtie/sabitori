//! Detect the Windows system color scheme (light or dark mode) and accent
//! color so the settings UI can match it.
//!
//! The registry key `HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\
//! Personalize\AppsUseLightTheme` controls whether apps use a light or
//! dark scheme. 0 = dark, 1 = light. If the key is missing (older Windows
//! or personalization disabled), dark mode is assumed as the default:
//! matching the app's original design.
//!
//! The accent color is read from `HKCU\Software\Microsoft\Windows\
//! CurrentVersion\Explorer\Accent\AccentPalette`, a binary blob of 8
//! DWORDs (0x00BBGGRR). The first is the light-mode accent, the fourth
//! is the dark-mode accent. If the key is missing, a default blue is used.

use windows::core::PCWSTR;
use windows::Win32::System::Registry::{
    RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
};
use windows::Win32::Foundation::ERROR_SUCCESS;

use crate::wide::to_wide;

const THEME_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize";
const APPS_USE_LIGHT: &str = "AppsUseLightTheme";
const ACCENT_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Accent";
const ACCENT_PALETTE: &str = "AccentPalette";

/// Default accent colors used when the registry key is missing.
const DEFAULT_LIGHT_ACCENT: [u8; 3] = [158, 232, 254]; // #9EE8FE
const DEFAULT_DARK_ACCENT: [u8; 3] = [2, 116, 217]; // #0274D9

/// Whether the system is using a light color scheme.
pub fn is_light_mode() -> bool {
    unsafe {
        let subkey = to_wide(THEME_KEY);
        let mut handle = HKEY::default();
        let result = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            None,
            KEY_READ,
            &mut handle,
        );
        if result != ERROR_SUCCESS {
            return false; // default to dark
        }

        let name = to_wide(APPS_USE_LIGHT);
        let mut value: u32 = 0;
        let mut buf_len = std::mem::size_of::<u32>() as u32;
        let result = RegQueryValueExW(
            handle,
            PCWSTR(name.as_ptr()),
            None,
            None,
            Some(&mut value as *mut _ as *mut _),
            Some(&mut buf_len),
        );
        let _ = RegCloseKey(handle);

        if result != ERROR_SUCCESS {
            return false;
        }

        value != 0
    }
}

/// Read the Windows system accent color as RGB bytes. Picks the light or
/// dark variant from the AccentPalette based on the current system mode.
/// Falls back to a default blue if the registry key is missing.
pub fn accent_color() -> [u8; 3] {
    let want_light = is_light_mode();
    let default = if want_light {
        DEFAULT_LIGHT_ACCENT
    } else {
        DEFAULT_DARK_ACCENT
    };

    unsafe {
        let subkey = to_wide(ACCENT_KEY);
        let mut handle = HKEY::default();
        let result = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            None,
            KEY_READ,
            &mut handle,
        );
        if result != ERROR_SUCCESS {
            return default;
        }

        let name = to_wide(ACCENT_PALETTE);
        // AccentPalette is up to 8 DWORDs = 32 bytes.
        let mut buf = [0u8; 32];
        let mut buf_len = buf.len() as u32;
        let result = RegQueryValueExW(
            handle,
            PCWSTR(name.as_ptr()),
            None,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut buf_len),
        );
        let _ = RegCloseKey(handle);

        if result != ERROR_SUCCESS || buf_len < 16 {
            return default;
        }

        // Each DWORD is 0x00BBGGRR (little-endian). Index 0 = light accent,
        // index 3 = dark accent.
        let idx = if want_light { 0 } else { 3 };
        let offset = idx * 4;
        let r = buf[offset];
        let g = buf[offset + 1];
        let b = buf[offset + 2];
        [r, g, b]
    }
}

/// The system accent color as a hex string (e.g. "#0274D9").
#[allow(dead_code)]
pub fn accent_hex() -> String {
    let [r, g, b] = accent_color();
    format!("#{r:02X}{g:02X}{b:02X}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accent_hex_is_valid_format() {
        let hex = accent_hex();
        assert!(hex.starts_with('#'));
        assert_eq!(hex.len(), 7);
        // Should parse as valid hex.
        let r = u8::from_str_radix(&hex[1..3], 16);
        let g = u8::from_str_radix(&hex[3..5], 16);
        let b = u8::from_str_radix(&hex[5..7], 16);
        assert!(r.is_ok() && g.is_ok() && b.is_ok());
    }

    #[test]
    fn accent_color_returns_nonzero() {
        // The accent color should not be pure black (0,0,0), Windows
        // always has some accent color set.
        let [r, g, b] = accent_color();
        assert!(r > 0 || g > 0 || b > 0);
    }
}
