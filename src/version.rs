//! Version reporting for `--version`.
//!
//! `log.yaml` is the file that already records the version, so it stays the
//! first source of truth — that is what makes `--version` agree with the
//! release notes. Because `log.yaml` is *user* configuration, a binary that
//! ships without one must still answer, which is what the compile-time
//! fallbacks are for.

use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use crate::settings::read_text;

/// File that carries the `version:` key.
pub const VERSION_FILE: &str = "log.yaml";

/// Shown when nothing at all can be determined.
pub const FALLBACK_VERSION: &str = "unknown";

#[derive(Deserialize)]
struct VersionFile {
    #[serde(default)]
    version: Option<Value>,
}

/// Returns the version for `--version`.
///
/// Order: `version:` in `<dir>/log.yaml`, then the version baked in at build
/// time (`NGINXY_BUILD_VERSION`), then this crate's own version.
pub fn read(dir: &Path) -> String {
    if let Some(version) = from_log_yaml(&dir.join(VERSION_FILE)) {
        return version;
    }

    // `build.rs` is free to export NGINXY_BUILD_VERSION; without it the crate
    // version is the honest answer.
    let build = option_env!("NGINXY_BUILD_VERSION")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());

    if build.is_empty() {
        FALLBACK_VERSION.to_string()
    } else {
        build
    }
}

fn from_log_yaml(path: &Path) -> Option<String> {
    let text = read_text(path).ok()?;
    let parsed: VersionFile = serde_norway::from_str(&text).ok()?;
    // An unquoted `version: 1.1` parses as a float, so stringify what came back.
    match parsed.version? {
        Value::String(text) => (!text.is_empty()).then_some(text),
        other => {
            let rendered = other.to_string();
            (!rendered.is_empty()).then_some(rendered)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp dir");
        for (name, body) in files {
            fs::write(dir.path().join(name), body).expect("write fixture");
        }
        dir
    }

    #[test]
    fn reads_version_from_log_yaml() {
        let dir = temp_dir(&[("log.yaml", "version: 0.1.0\nlogging:\n  folder: logs\n")]);
        assert_eq!(read(dir.path()), "0.1.0");
    }

    #[test]
    fn reads_version_with_bom_and_quotes() {
        let dir = temp_dir(&[("log.yaml", "\u{feff}version: \"1.1.0\"\n")]);
        assert_eq!(read(dir.path()), "1.1.0");
    }

    #[test]
    fn unquoted_numeric_version_is_stringified() {
        let dir = temp_dir(&[("log.yaml", "version: 1.1\n")]);
        assert_eq!(read(dir.path()), "1.1");
    }

    #[test]
    fn falls_back_to_crate_version_without_config() {
        let dir = temp_dir(&[]);
        assert_eq!(read(dir.path()), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn ignores_broken_yaml() {
        let dir = temp_dir(&[("log.yaml", "version: [unclosed\n")]);
        assert_eq!(read(dir.path()), env!("CARGO_PKG_VERSION"));
    }
}
