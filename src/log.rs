//! Minimal file logger for a console-less GUI app.
//!
//! Errors are appended to `sabitori.log` next to the executable so they are not
//! silently lost now that the app runs without a console window.
//!
//! The file is opened once, at the first log call, and written through a
//! buffered writer for the life of the process, the old per-call
//! open/append/close cycle caused thousands of open/write/close calls per
//! second under the Debug Logging wheel traces. A background thread flushes
//! the buffer about once per second (plus `BufWriter`'s automatic overflow
//! flush), keeping the data-loss tail on a hard kill small, and the app also
//! flushes at its clean shutdown points. At init an oversized log is
//! truncated so the file cannot grow unbounded.

use std::fs::{self, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The log is capped at roughly 1 MiB; on init an oversized file is truncated
/// so it cannot grow unbounded.
const MAX_LOG_BYTES: u64 = 1024 * 1024;
/// How often the background flusher pushes buffered lines to disk. Keeping
/// this small bounds the data-loss tail on a hard kill.
const FLUSH_INTERVAL: Duration = Duration::from_millis(1000);

/// Path to the log file, next to the executable (same convention as config).
fn log_path() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("sabitori.exe"));
    let dir = exe.parent().unwrap_or_else(|| std::path::Path::new("."));
    dir.join("sabitori.log")
}

/// The process-lifetime log file: a buffered writer opened once in append
/// mode. Opening truncates an oversized file, so the log cannot grow
/// unbounded.
struct LogFile {
    writer: Box<dyn Write + Send>,
}

impl LogFile {
    /// Open the log at `path` for appending, truncating it first if it has
    /// grown past the cap. A missing file is created.
    ///
    /// Truncation needs a separate write-only handle: an append handle on
    /// Windows gets only `FILE_APPEND_DATA` access, which forbids `set_len`
    /// (it needs `FILE_WRITE_DATA`). So an oversized file is truncated here,
    /// before the append handle for the writer is opened.
    fn open(path: &Path) -> io::Result<Self> {
        let oversized = fs::metadata(path)
            .map(|m| m.len() > MAX_LOG_BYTES)
            .unwrap_or(false);
        if oversized {
            let truncate = OpenOptions::new().write(true).open(path)?;
            truncate.set_len(0)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            writer: Box::new(BufWriter::new(file)),
        })
    }

    /// A writer that discards everything, used when the log file cannot be
    /// opened, so logging never panics or blocks.
    fn discarding() -> Self {
        Self {
            writer: Box::new(BufWriter::new(io::sink())),
        }
    }

    /// Append one timestamped line. The format is identical to the old
    /// per-call logger. Lines are written in call order; failures are ignored
    /// (there is nowhere else to report them).
    fn write_line(&mut self, msg: &str) {
        // Millisecond precision so timing-sensitive traces (e.g. the
        // direction-lock timeout) are easy to correlate. Written directly
        // to the buffer to avoid a String allocation per log call (the
        // debug wheel traces can produce thousands per second).
        let ts = SystemTime::now().duration_since(UNIX_EPOCH);
        let _ = write!(self.writer, "[");
        match ts {
            Ok(d) => {
                let _ = write!(self.writer, "{}.{:03}", d.as_secs(), d.subsec_millis());
            }
            Err(_) => {
                let _ = write!(self.writer, "0.000");
            }
        }
        let _ = writeln!(self.writer, "] {msg}");
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_file_open_creates_missing() {
        let path = std::env::temp_dir().join("sabitori_test_log_create.log");
        let _ = fs::remove_file(&path);
        let _lf = LogFile::open(&path).expect("open should succeed");
        assert!(path.exists(), "log file should be created on open");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn log_file_write_and_flush() {
        let path = std::env::temp_dir().join("sabitori_test_log_write.log");
        let _ = fs::remove_file(&path);
        let mut lf = LogFile::open(&path).expect("open should succeed");
        lf.write_line("hello world");
        lf.flush().expect("flush should succeed");
        let contents = fs::read_to_string(&path).expect("file should be readable");
        assert!(contents.contains("hello world"), "line should be in the file");
        // Timestamp format: [seconds.millis] message
        assert!(contents.starts_with('['), "line should start with timestamp bracket");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn log_file_appends_existing() {
        let path = std::env::temp_dir().join("sabitori_test_log_append.log");
        let _ = fs::remove_file(&path);
        // First write.
        let mut lf1 = LogFile::open(&path).expect("open should succeed");
        lf1.write_line("first");
        lf1.flush().expect("flush should succeed");
        drop(lf1);
        // Second write should append, not overwrite.
        let mut lf2 = LogFile::open(&path).expect("open should succeed");
        lf2.write_line("second");
        lf2.flush().expect("flush should succeed");
        let contents = fs::read_to_string(&path).expect("file should be readable");
        assert!(contents.contains("first"), "first line should survive");
        assert!(contents.contains("second"), "second line should be appended");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn log_file_truncates_oversized() {
        let path = std::env::temp_dir().join("sabitori_test_log_truncate.log");
        let _ = fs::remove_file(&path);
        // Write a file larger than MAX_LOG_BYTES.
        let big = "x".repeat(MAX_LOG_BYTES as usize + 100);
        fs::write(&path, &big).expect("write should succeed");
        let meta = fs::metadata(&path).expect("metadata should succeed");
        assert!(meta.len() > MAX_LOG_BYTES);
        // Opening should truncate it.
        let _lf = LogFile::open(&path).expect("open should succeed");
        let meta = fs::metadata(&path).expect("metadata should succeed");
        assert_eq!(meta.len(), 0, "oversized file should be truncated");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn log_file_discarding_never_panics() {
        let mut lf = LogFile::discarding();
        lf.write_line("this goes nowhere");
        let _ = lf.flush();
        // No panic, no error, that's the contract.
    }
}

/// The process-lifetime writer, lazily initialized on the first log call.
static WRITER: OnceLock<Mutex<LogFile>> = OnceLock::new();
/// Guards the one-time spawn of the background flusher thread.
static FLUSHER_SPAWNED: OnceLock<()> = OnceLock::new();

/// Append a timestamped line to the log file.
pub fn log(msg: &str) {
    // Lazily start the background flusher and open the log (truncating an
    // oversized one), once, on first use.
    FLUSHER_SPAWNED.get_or_init(spawn_flusher);
    let writer = WRITER.get_or_init(|| {
        Mutex::new(LogFile::open(&log_path()).unwrap_or_else(|_| LogFile::discarding()))
    });
    if let Ok(mut guard) = writer.lock() {
        guard.write_line(msg);
    }
}

/// Push buffered lines to disk. Called from the app's clean shutdown points
/// and by the background flusher; a no-op before the first log call.
pub fn flush() {
    if let Some(writer) = WRITER.get() {
        if let Ok(mut guard) = writer.lock() {
            let _ = guard.flush();
        }
    }
}

/// Spawn the detached thread that flushes the buffer about once per second.
/// A spawn failure is ignored, flushing then falls back to `BufWriter`'s
/// overflow flush plus the app's shutdown flushes.
fn spawn_flusher() {
    let _ = std::thread::Builder::new()
        .name("sabitori-log-flusher".to_string())
        .spawn(|| loop {
            std::thread::sleep(FLUSH_INTERVAL);
            flush();
        });
}
