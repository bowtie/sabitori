use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// How physical mouse wheel events are handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WheelMode {
    /// Swallow all physical wheel events (scroll lock).
    Disable,
    /// Filter spurious opposite-direction ticks (default).
    #[default]
    DirectionLock,
    /// Pass the wheel through untouched.
    Off,
}

impl WheelMode {
    /// Integer encoding for atomic storage (0=Disable, 1=DirectionLock, 2=Off).
    pub fn as_i32(self) -> i32 {
        match self {
            Self::Disable => 0,
            Self::DirectionLock => 1,
            Self::Off => 2,
        }
    }

    pub fn from_i32(v: i32) -> Self {
        match v {
            0 => Self::Disable,
            2 => Self::Off,
            _ => Self::DirectionLock,
        }
    }

    /// The display name as a `&'static str`, for use in hot paths that
    /// must not allocate (e.g. GPUI render). `Display` delegates here.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Disable => "Scroll Lock",
            Self::DirectionLock => "Direction Lock",
            Self::Off => "Pass Through",
        }
    }
}

impl fmt::Display for WheelMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.display_name())
    }
}

/// Persistent application settings stored as JSON on disk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// How physical wheel events are handled.
    #[serde(default)]
    pub wheel_mode: WheelMode,
    /// Direction-lock idle timeout in milliseconds (Mode B only). After the
    /// wheel has been silent this long, the lock releases so a deliberate
    /// reversal takes effect immediately. During continuous scrolling,
    /// opposite-direction ticks are instead suppressed until 2 consecutive
    /// ones arrive (a real direction change).
    #[serde(default = "default_dir_lock_timeout_ms")]
    pub direction_lock_timeout_ms: u32,
}

/// Default for `direction_lock_timeout_ms`, used by serde when the field
/// is missing from the JSON. `u32::default()` would be 0, which clamps to
/// the minimum (50ms); this preserves the real default (500ms).
fn default_dir_lock_timeout_ms() -> u32 {
    Config::default().direction_lock_timeout_ms
}

impl Default for Config {
    fn default() -> Self {
        Self {
            wheel_mode: WheelMode::DirectionLock,
            direction_lock_timeout_ms: 500,
        }
    }
}

/// Valid range for the direction-lock timeout. Shared with the settings-
/// window slider, so the UI and the on-load clamping can never drift apart.
pub const DIR_LOCK_TIMEOUT_MIN_MS: u32 = 50;
pub const DIR_LOCK_TIMEOUT_MAX_MS: u32 = 1000;

impl Config {
    /// Clamp numeric fields to their valid ranges.
    fn sanitized(mut self) -> Self {
        self.direction_lock_timeout_ms = self
            .direction_lock_timeout_ms
            .clamp(DIR_LOCK_TIMEOUT_MIN_MS, DIR_LOCK_TIMEOUT_MAX_MS);
        self
    }
}

/// Returns the path to the config file next to the executable.
fn config_path() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("sabitori.exe"));
    let dir = exe.parent().unwrap_or_else(|| std::path::Path::new("."));
    dir.join("config.json")
}

/// Load config from disk, falling back to defaults if missing or invalid.
pub fn load() -> Config {
    load_at(&config_path())
}

/// Load config from `path`. A missing file is normal (first launch) and stays
/// silent; a parse failure (corrupt or hand-edited file) falls back to
/// defaults but is logged so silent data loss becomes diagnosable.
fn load_at(path: &Path) -> Config {
    let config = match fs::read_to_string(path) {
        Ok(contents) => match serde_json::from_str(&contents) {
            Ok(config) => config,
            Err(e) => {
                crate::log::log(&format!(
                    "Failed to parse config ({}), falling back to defaults: {e}",
                    path.display()
                ));
                Config::default()
            }
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => Config::default(),
        Err(e) => {
            crate::log::log(&format!(
                "Failed to read config ({}), falling back to defaults: {e}",
                path.display()
            ));
            Config::default()
        }
    };
    config.sanitized()
}

/// Save config to disk, atomically: serialize to a temp file in the same
/// directory, then rename it over the target. `fs::rename` replaces the
/// destination atomically (`MOVEFILE_REPLACE_EXISTING` on Windows), so a
/// crash mid-save can never leave a truncated config: readers see either
/// the old complete file or the new complete file, never a partial one.
pub fn save(config: &Config) -> io::Result<()> {
    save_at(config, &config_path())
}

fn save_at(config: &Config, path: &Path) -> io::Result<()> {
    let contents = serde_json::to_string_pretty(config)?;
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp = dir.join(format!(
        "{}.tmp",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("config.json")
    ));
    fs::write(&tmp, contents)?;
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            // Best effort: don't leave the temp file behind on failure.
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wheel_mode_roundtrip() {
        for mode in [WheelMode::Disable, WheelMode::DirectionLock, WheelMode::Off] {
            assert_eq!(WheelMode::from_i32(mode.as_i32()), mode);
        }
    }

    #[test]
    fn wheel_mode_from_i32_fallback() {
        // Unknown values fall back to DirectionLock (the default).
        assert_eq!(WheelMode::from_i32(999), WheelMode::DirectionLock);
        assert_eq!(WheelMode::from_i32(-1), WheelMode::DirectionLock);
    }

    #[test]
    fn sanitized_clamps_timeout() {
        let cfg = Config {
            wheel_mode: WheelMode::DirectionLock,
            direction_lock_timeout_ms: 0,
            ..Default::default()
        };
        let s = cfg.sanitized();
        assert_eq!(s.direction_lock_timeout_ms, DIR_LOCK_TIMEOUT_MIN_MS);

        let cfg = Config {
            wheel_mode: WheelMode::DirectionLock,
            direction_lock_timeout_ms: 999_999,
            ..Default::default()
        };
        let s = cfg.sanitized();
        assert_eq!(s.direction_lock_timeout_ms, DIR_LOCK_TIMEOUT_MAX_MS);
    }

    #[test]
    fn sanitized_preserves_valid_timeout() {
        let cfg = Config {
            wheel_mode: WheelMode::DirectionLock,
            direction_lock_timeout_ms: 500,
            ..Default::default()
        };
        let s = cfg.sanitized();
        assert_eq!(s.direction_lock_timeout_ms, 500);
    }

    #[test]
    fn config_json_roundtrip() {
        let cfg = Config {
            wheel_mode: WheelMode::Off,
            direction_lock_timeout_ms: 250,
            ..Default::default()
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let parsed: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, parsed);
    }

    #[test]
    fn default_config_values() {
        let d = Config::default();
        assert_eq!(d.wheel_mode, WheelMode::DirectionLock);
        assert_eq!(d.direction_lock_timeout_ms, 500);
    }

    #[test]
    fn wheel_mode_display() {
        assert_eq!(WheelMode::Disable.to_string(), "Scroll Lock");
        assert_eq!(WheelMode::DirectionLock.to_string(), "Direction Lock");
        assert_eq!(WheelMode::Off.to_string(), "Pass Through");
    }

    #[test]
    fn wheel_mode_display_name_static() {
        // display_name returns &'static str, verify the values match Display.
        assert_eq!(WheelMode::Disable.display_name(), "Scroll Lock");
        assert_eq!(WheelMode::DirectionLock.display_name(), "Direction Lock");
        assert_eq!(WheelMode::Off.display_name(), "Pass Through");
    }

    #[test]
    fn save_load_roundtrip() {
        let dir = std::env::temp_dir();
        let path = dir.join("sabitori_test_config.json");
        // Clean up any leftover from a previous run.
        let _ = fs::remove_file(&path);

        let cfg = Config {
            wheel_mode: WheelMode::Off,
            direction_lock_timeout_ms: 750,
            ..Default::default()
        };
        save_at(&cfg, &path).expect("save should succeed");
        let loaded = load_at(&path);
        assert_eq!(loaded, cfg);

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn load_missing_file_returns_defaults() {
        let path = std::env::temp_dir().join("sabitori_nonexistent_config.json");
        let _ = fs::remove_file(&path);
        let loaded = load_at(&path);
        assert_eq!(loaded, Config::default());
    }

    #[test]
    fn load_corrupt_file_returns_defaults() {
        let path = std::env::temp_dir().join("sabitori_corrupt_config.json");
        fs::write(&path, "{ this is not valid json }").unwrap();
        let loaded = load_at(&path);
        assert_eq!(loaded, Config::default());
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn load_partial_config_uses_defaults_for_missing_fields() {
        // With #[serde(default)] on both fields, a JSON object missing a
        // field deserializes successfully, using the default for the
        // missing field. An unknown mode name falls back to the default.
        let path = std::env::temp_dir().join("sabitori_partial_config.json");
        fs::write(&path, r#"{"wheel_mode": "nonexistent"}"#).unwrap();
        let loaded = load_at(&path);
        assert_eq!(loaded.wheel_mode, WheelMode::DirectionLock);
        // Missing direction_lock_timeout_ms uses the serde default (500ms).
        assert_eq!(loaded.direction_lock_timeout_ms, 500);
        let _ = fs::remove_file(&path);

        // Also test the reverse: only timeout specified, mode missing.
        fs::write(&path, r#"{"direction_lock_timeout_ms": 750}"#).unwrap();
        let loaded = load_at(&path);
        assert_eq!(loaded.wheel_mode, WheelMode::DirectionLock); // default
        assert_eq!(loaded.direction_lock_timeout_ms, 750);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn save_overwrites_existing_file() {
        let path = std::env::temp_dir().join("sabitori_overwrite_config.json");
        let _ = fs::remove_file(&path);

        // Save once.
        let cfg1 = Config {
            wheel_mode: WheelMode::DirectionLock,
            direction_lock_timeout_ms: 100,
            ..Default::default()
        };
        save_at(&cfg1, &path).expect("first save");
        // Save again with different values.
        let cfg2 = Config {
            wheel_mode: WheelMode::Off,
            direction_lock_timeout_ms: 750,
            ..Default::default()
        };
        save_at(&cfg2, &path).expect("second save");
        // The loaded config should match the second save.
        let loaded = load_at(&path);
        assert_eq!(loaded, cfg2);
        let _ = fs::remove_file(&path);
    }
}
