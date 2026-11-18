//! Version reporting for `--version`.
//!
//! `--version` always reports the version of the *binary that is running*, so
//! it answers "which build did I install?" without being influenced by the
//! configuration in the folder. The `version:` key in `log.yaml` is a separate
//! marker that the release tooling anchors on (`cargo xtask release-notes`);
//! it is deliberately not what `--version` prints.

use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use crate::settings::read_text;

/// File that carries the `version:` key.
pub const VERSION_FILE: &str = "log.yaml";

/// Shown when the built binary carries no version at all.
pub const FALLBACK_VERSION: &str = "unknown";

#[derive(Deserialize)]
struct VersionFile {
    #[serde(default)]
    version: Option<Value>,
}

/// The version baked into this binary: `NGINXY_BUILD_VERSION` if a build step
/// set it, else the crate version from `Cargo.toml`. Always what `--version`
/// prints.
pub fn binary() -> String {
    let version = option_env!("NGINXY_BUILD_VERSION")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .unwrap_or(env!("CARGO_PKG_VERSION").to_string());

    if version.is_empty() {
        FALLBACK_VERSION.to_string()
    } else {
        version
    }
}

/// Reads the `version:` key from `<dir>/log.yaml`, if the file carries one.
pub fn from_log_yaml(dir: &Path) -> Option<String> {
    let text = read_text(&dir.join(VERSION_FILE)).ok()?;
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

/// For the release tooling (`cargo xtask release-notes`): the `log.yaml`
/// marker if present, else the binary's own version. Not used by `--version`.
pub fn read(dir: &Path) -> String {
    from_log_yaml(dir).unwrap_or_else(binary)
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

    #[test]
    fn binary_is_the_crate_version() {
        assert_eq!(binary(), env!("CARGO_PKG_VERSION"));
    }
}
