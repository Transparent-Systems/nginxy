//! Logging: the same one-line format, the same rotating file, the same place.
//!
//! Python used a `RotatingFileHandler` plus a `StreamHandler` with a shared
//! formatter (`main.py:67-115`). `env_logger` and `tracing-subscriber` cannot
//! reproduce either the line format or the rotation rules without more
//! configuration than writing the writer, so this module implements `log::Log`
//! directly. Everything it emits goes to **stderr** and to the log file, in
//! Python's format:
//!
//! ```text
//! 2026-11-18 08:12:33,123 - INFO - Starting Sync: 2 targets configured.
//! ```

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Local};
use log::{Level, LevelFilter, Log, Metadata, Record};

/// Python's level names (`WARNING`, not `WARN`).
pub fn level_name(level: Level) -> &'static str {
    match level {
        Level::Error => "ERROR",
        Level::Warn => "WARNING",
        Level::Info => "INFO",
        Level::Debug => "DEBUG",
        Level::Trace => "TRACE",
    }
}

/// Formats one record exactly like `'%(asctime)s - %(levelname)s - %(message)s'`.
pub fn format_record_at(timestamp: DateTime<Local>, level: Level, message: &str) -> String {
    format!(
        "{},{:03} - {} - {}",
        timestamp.format("%Y-%m-%d %H:%M:%S"),
        timestamp.timestamp_subsec_millis(),
        level_name(level),
        message
    )
}

/// [`format_record_at`] for "now".
pub fn format_record(level: Level, message: &str) -> String {
    format_record_at(Local::now(), level, message)
}

/// Append-only file that rotates the way `RotatingFileHandler` does.
///
/// Rotation happens *before* a write that would reach `max_bytes`, renames
/// `name` to `name.1` (shifting `name.1` to `name.2` and so on) and drops the
/// oldest backup beyond `max_backups`. `max_bytes == 0` disables rotation, as
/// in Python.
#[derive(Debug)]
pub struct RotatingFile {
    path: PathBuf,
    file: File,
    written: u64,
    max_bytes: u64,
    max_backups: u32,
}

impl RotatingFile {
    pub fn open(path: &Path, max_bytes: u64, max_backups: u32) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let written = file.metadata()?.len();
        Ok(Self {
            path: path.to_path_buf(),
            file,
            written,
            max_bytes,
            max_backups,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn write_line(&mut self, line: &str) -> io::Result<()> {
        let mut record = Vec::with_capacity(line.len() + 1);
        record.extend_from_slice(line.as_bytes());
        record.push(b'\n');

        if self.max_bytes > 0 && self.written + record.len() as u64 >= self.max_bytes {
            self.rotate()?;
        }

        self.file.write_all(&record)?;
        self.file.flush()?;
        self.written += record.len() as u64;
        Ok(())
    }

    pub fn flush(&mut self) {
        let _ = self.file.flush();
    }

    fn backup_path(&self, index: u32) -> PathBuf {
        let mut name = self.path.clone().into_os_string();
        name.push(format!(".{index}"));
        PathBuf::from(name)
    }

    fn rotate(&mut self) -> io::Result<()> {
        if self.max_backups > 0 {
            for index in (1..self.max_backups).rev() {
                let from = self.backup_path(index);
                let to = self.backup_path(index + 1);
                if from.exists() {
                    if to.exists() {
                        fs::remove_file(&to)?;
                    }
                    fs::rename(&from, &to)?;
                }
            }

            let first = self.backup_path(1);
            if first.exists() {
                fs::remove_file(&first)?;
            }
            fs::rename(&self.path, &first)?;
        }

        self.file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        self.written = 0;
        Ok(())
    }
}

/// Writes one line to stderr without panicking.
///
/// `eprintln!` panics when the write fails — which is what happens when the
/// output is piped into something that exits early (`nginxy … | head`). Losing
/// a console line is not worth aborting a sync.
fn write_console(line: &str) {
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(stderr, "{line}");
}

struct FileLogger {
    file: Mutex<RotatingFile>,
}

struct ConsoleLogger;

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format_record(record.level(), &record.args().to_string());
        // Console first: an unwritable log file must not hide the message.
        write_console(&line);
        if let Ok(mut file) = self.file.lock() {
            let _ = file.write_line(&line);
        }
    }

    fn flush(&self) {
        if let Ok(mut file) = self.file.lock() {
            file.flush();
        }
    }
}

impl Log for ConsoleLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &Record<'_>) {
        if self.enabled(record.metadata()) {
            write_console(&format_record(record.level(), &record.args().to_string()));
        }
    }

    fn flush(&self) {}
}

/// Installs the file+console logger. Call once per process.
pub fn init(level: LevelFilter, path: &Path, max_bytes: u64, max_backups: u32) -> io::Result<()> {
    let file = RotatingFile::open(path, max_bytes, max_backups)?;
    let _ = log::set_boxed_logger(Box::new(FileLogger {
        file: Mutex::new(file),
    }));
    log::set_max_level(level);
    Ok(())
}

/// Fallback when the log file cannot be opened: stderr still works.
pub fn init_console_only(level: LevelFilter) {
    let _ = log::set_boxed_logger(Box::new(ConsoleLogger));
    log::set_max_level(level);
}

/// Changes the level of an already installed logger (`-l` handling).
pub fn set_level(level: LevelFilter) {
    log::set_max_level(level);
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::fs;

    fn at(second: u32, millis: u32) -> DateTime<Local> {
        let base = Local
            .with_ymd_and_hms(2026, 11, 18, 8, 12, second)
            .single()
            .expect("valid local time");
        base + chrono::Duration::milliseconds(millis as i64)
    }

    #[test]
    fn line_format_matches_python() {
        let line = format_record_at(
            at(33, 123),
            Level::Info,
            "Starting Sync: 2 targets configured.",
        );
        assert_eq!(
            line,
            "2026-11-18 08:12:33,123 - INFO - Starting Sync: 2 targets configured."
        );
    }

    #[test]
    fn level_names_use_python_spelling() {
        assert_eq!(level_name(Level::Warn), "WARNING");
        assert_eq!(level_name(Level::Error), "ERROR");
        assert_eq!(level_name(Level::Debug), "DEBUG");
    }

    #[test]
    fn appends_newline_and_creates_parent_folders() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("logs").join("nginxy.log");
        let mut file = RotatingFile::open(&path, 0, 3).expect("open");

        file.write_line("first").expect("write");
        file.write_line("second").expect("write");

        assert_eq!(fs::read_to_string(&path).unwrap(), "first\nsecond\n");
        assert_eq!(file.path(), path.as_path());
    }

    #[test]
    fn rotates_before_exceeding_max_bytes_and_keeps_backups() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("nginxy.log");
        // Each record is 4 bytes ("abc\n"); a 10-byte budget allows two.
        let mut file = RotatingFile::open(&path, 10, 2).expect("open");

        file.write_line("abc").expect("write");
        file.write_line("abc").expect("write");
        // The third record would reach the limit, so "abc\nabc\n" moves to .1.
        file.write_line("abc").expect("write");

        assert_eq!(fs::read_to_string(&path).unwrap(), "abc\n");
        assert_eq!(
            fs::read_to_string(path.with_extension("log.1")).unwrap(),
            "abc\nabc\n"
        );

        file.write_line("abc").expect("write");
        file.write_line("abc").expect("write");
        // Second rotation shifts .1 to .2 and drops anything older.
        assert!(path.with_extension("log.2").exists());
        assert!(!path.with_extension("log.3").exists());
    }

    #[test]
    fn zero_max_bytes_disables_rotation() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("nginxy.log");
        let mut file = RotatingFile::open(&path, 0, 3).expect("open");
        for _ in 0..50 {
            file.write_line("a record long enough to matter")
                .expect("write");
        }
        assert!(!path.with_extension("log.1").exists());
    }
}
