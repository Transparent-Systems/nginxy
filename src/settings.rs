//! Configuration: `config.yaml`, `log.yaml`, `.env` — the same three files, in
//! the same folder, with the same defaults as the Python application.
//!
//! Differences from `main.py` are structural only: the Python module kept the
//! loaded values in mutable globals (`SETTINGS_DIR`, `CONFIG`, `TARGETS`,
//! `logger`) because the config folder is itself a command-line option. Here
//! they live in one [`Settings`] value that is loaded after argument parsing
//! and passed by reference, so there is nothing to keep in sync.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use log::LevelFilter;
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::template;

/// Environment variables that supply the login for every endpoint without its
/// own `auth_payload`.
pub const ENV_IDENTITY: &str = "NPM_SOURCE_IDENTITY";
pub const ENV_SECRET: &str = "NPM_SOURCE_SECRET";

/// File names. Identical to the Python app's, so an existing folder works.
pub const CONFIG_FILE: &str = "config.yaml";
pub const LOG_CONFIG_FILE: &str = "log.yaml";
pub const ENV_FILE: &str = ".env";

#[derive(Debug)]
pub enum SettingsError {
    /// `config.yaml` is not there; the CLI turns this into the three-line hint.
    MissingConfig {
        path: PathBuf,
    },
    Read {
        path: PathBuf,
        source: io::Error,
    },
    Parse {
        path: PathBuf,
        source: serde_norway::Error,
    },
    /// `source.url` is absent or blank.
    NoSourceUrl {
        path: PathBuf,
    },
    /// A `${VAR}` placeholder could not be resolved.
    Template {
        path: PathBuf,
        message: String,
    },
    /// The YAML parsed, but its shape does not match the configuration model.
    Model {
        path: PathBuf,
        source: serde_json::Error,
    },
}

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SettingsError::MissingConfig { path } => {
                write!(f, "configuration file not found: {}", path.display())
            }
            SettingsError::Read { path, source } => {
                write!(f, "cannot read {}: {source}", path.display())
            }
            SettingsError::Parse { path, source } => {
                write!(f, "cannot parse {}: {source}", path.display())
            }
            SettingsError::NoSourceUrl { path } => write!(
                f,
                "no source url configured in {} (expected `source: {{url: ...}}`)",
                path.display()
            ),
            SettingsError::Template { path, message } => {
                write!(f, "{}: {message}", path.display())
            }
            SettingsError::Model { path, source } => {
                write!(f, "cannot use {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for SettingsError {}

/// `config.yaml`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub source: Option<SourceConfig>,
    #[serde(default)]
    pub targets: Vec<TargetConfig>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SourceConfig {
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub auth_payload: Option<AuthPayload>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct TargetConfig {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub auth_payload: Option<AuthPayload>,
}

/// `auth_payload` as written in `config.yaml`.
///
/// The Python app accepts both a YAML mapping and a JSON string
/// (`main.py:344-352`), so both are accepted here.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum AuthPayload {
    Mapping(Map<String, Value>),
    Json(String),
}

/// Where the identity/secret came from after resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Auth {
    /// A complete `{"identity": ..., "secret": ...}` body.
    Payload(Value),
    /// `auth_payload` was a string that is not valid JSON.
    InvalidJson,
    /// Neither `auth_payload` nor the environment supplied both values.
    Missing,
}

impl Auth {
    pub fn is_usable(&self) -> bool {
        matches!(self, Auth::Payload(_))
    }

    pub fn payload(&self) -> Option<Value> {
        match self {
            Auth::Payload(value) => Some(value.clone()),
            Auth::InvalidJson | Auth::Missing => None,
        }
    }
}

/// The `NPM_SOURCE_IDENTITY`/`NPM_SOURCE_SECRET` pair, as seen by one load.
///
/// Kept as a value instead of reading `std::env` on demand so that a `.env`
/// file is parsed without mutating the process environment (python-dotenv does
/// mutate it, which makes behaviour depend on the order files were loaded in).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvCredentials {
    pub identity: Option<String>,
    pub secret: Option<String>,
}

impl EnvCredentials {
    pub fn is_complete(&self) -> bool {
        self.identity.is_some() && self.secret.is_some()
    }

    pub fn payload(&self) -> Option<Value> {
        match (&self.identity, &self.secret) {
            (Some(identity), Some(secret)) => {
                let mut map = Map::new();
                map.insert("identity".into(), Value::String(identity.clone()));
                map.insert("secret".into(), Value::String(secret.clone()));
                Some(Value::Object(map))
            }
            _ => None,
        }
    }
}

/// Resolves the login body for one endpoint.
///
/// Mirrors `get_auth_payload()`: the config wins, the environment is the
/// fallback, and an *empty* `auth_payload` counts as absent (Python's
/// `auth_payload or {env}` treats `{}` as falsy).
pub fn resolve_auth(payload: Option<&AuthPayload>, env: &EnvCredentials) -> Auth {
    match payload {
        Some(AuthPayload::Mapping(map)) if !map.is_empty() => {
            Auth::Payload(Value::Object(map.clone()))
        }
        Some(AuthPayload::Json(text)) => match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(map)) if !map.is_empty() => Auth::Payload(Value::Object(map)),
            Ok(_) => env_auth(env),
            Err(_) => Auth::InvalidJson,
        },
        Some(AuthPayload::Mapping(_)) | None => env_auth(env),
    }
}

fn env_auth(env: &EnvCredentials) -> Auth {
    match env.payload() {
        Some(value) => Auth::Payload(value),
        None => Auth::Missing,
    }
}

/// `logging:` from `log.yaml`, with the Python defaults.
#[derive(Debug, Clone, Deserialize)]
pub struct LoggingConfig {
    #[serde(default = "default_log_file_name")]
    pub file_name: String,
    #[serde(default = "default_log_folder")]
    pub folder: String,
    #[serde(default = "default_max_bytes")]
    pub max_bytes: u64,
    #[serde(default = "default_max_backup_count")]
    pub max_backup_count: u32,
    #[serde(default = "default_log_level")]
    pub log_level: String,
}

fn default_log_file_name() -> String {
    "nginx_sync.log".to_string()
}

fn default_log_folder() -> String {
    "logs".to_string()
}

fn default_max_bytes() -> u64 {
    10_485_760
}

fn default_max_backup_count() -> u32 {
    3
}

fn default_log_level() -> String {
    "INFO".to_string()
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            file_name: default_log_file_name(),
            folder: default_log_folder(),
            max_bytes: default_max_bytes(),
            max_backup_count: default_max_backup_count(),
            log_level: default_log_level(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct LogFileConfig {
    #[serde(default)]
    logging: Option<LoggingConfig>,
}

/// Everything the sync loop needs, loaded from one folder.
#[derive(Debug, Clone)]
pub struct Settings {
    /// Resolved configuration folder (`--config-dir`, else the binary's folder).
    pub dir: PathBuf,
    pub source: SourceConfig,
    pub targets: Vec<TargetConfig>,
    pub logging: LoggingConfig,
    /// Absolute path of the log file (`<dir>/<folder>/<file_name>`).
    pub log_path: PathBuf,
    /// Level from `log.yaml`, used unless `--log-level` is given.
    pub log_level: LevelFilter,
    /// Credentials captured at load time; see [`EnvCredentials`].
    pub env: EnvCredentials,
}

impl Settings {
    /// Reads `.env`, `config.yaml` and `log.yaml` from `dir`.
    pub fn load(dir: &Path) -> Result<Self, SettingsError> {
        let dir = dir.to_path_buf();
        // Environment first: `.env` fills the gaps, and `${VAR}` in the YAML
        // files resolves against the same map, so credentials and placeholders
        // can never disagree.
        let env_vars = load_env_map(&dir);
        let env = credentials_from(&env_vars);

        let config_path = dir.join(CONFIG_FILE);
        let text = match read_text(&config_path) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                return Err(SettingsError::MissingConfig { path: config_path })
            }
            Err(source) => {
                return Err(SettingsError::Read {
                    path: config_path,
                    source,
                })
            }
        };
        // Parse to a generic document first, then expand the values, then build
        // the typed model. Expanding the parsed document (instead of the raw
        // text) means placeholders in comments stay inert.
        let mut document: serde_json::Value =
            serde_norway::from_str(&text).map_err(|source| SettingsError::Parse {
                path: config_path.clone(),
                source,
            })?;
        template::expand_value(&mut document, &env_vars).map_err(|err| {
            SettingsError::Template {
                path: config_path.clone(),
                message: err.message(),
            }
        })?;
        let config: Config =
            serde_json::from_value(document).map_err(|source| SettingsError::Model {
                path: config_path.clone(),
                source,
            })?;

        let source = config.source.unwrap_or_default();
        if source.url.trim().is_empty() {
            return Err(SettingsError::NoSourceUrl { path: config_path });
        }

        let logging = read_logging_config(&dir.join(LOG_CONFIG_FILE), &env_vars);
        let log_path = resolve_log_path(&dir, &logging);
        let log_level = parse_level(&logging.log_level).unwrap_or(LevelFilter::Info);

        Ok(Self {
            dir,
            source,
            targets: config.targets,
            logging,
            log_path,
            log_level,
            env,
        })
    }

    /// Source or targets that cannot log in, by display name.
    ///
    /// The name for the source is `"source"`, exactly as the Python app reports
    /// it; a target without a `name` falls back to its URL.
    pub fn missing_credentials(&self) -> Vec<String> {
        self.missing_credentials_with(&self.env)
    }

    pub fn missing_credentials_with(&self, env: &EnvCredentials) -> Vec<String> {
        let mut missing = Vec::new();
        if !resolve_auth(self.source.auth_payload.as_ref(), env).is_usable() {
            missing.push("source".to_string());
        }
        for target in &self.targets {
            if !resolve_auth(target.auth_payload.as_ref(), env).is_usable() {
                missing.push(target_name(target));
            }
        }
        missing
    }
}

/// `target.get('name', target['url'])` — the name used in log lines.
pub fn target_name(target: &TargetConfig) -> String {
    target.name.clone().unwrap_or_else(|| target.url.clone())
}

/// Reads a UTF-8 text file, tolerating the BOM that Windows editors write.
///
/// Python opens all three configuration files with `encoding='utf-8-sig'`,
/// which strips the BOM; the YAML and `.env` parsers used here do not.
pub fn read_text(path: &Path) -> io::Result<String> {
    let bytes = fs::read(path)?;
    let text = String::from_utf8(bytes).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is not valid UTF-8", path.display()),
        )
    })?;
    Ok(match text.strip_prefix('\u{feff}') {
        Some(rest) => rest.to_string(),
        None => text,
    })
}

/// `logging:` level names to a `log` filter.
///
/// The five names Python's `logging` module uses. `CRITICAL` maps to `Error`:
/// the `log` crate has no level above error, so a CRITICAL setting keeps error
/// lines visible instead of hiding them. An unknown name (including the old
/// `NONE`) is reported as unrecognised, and the caller then keeps whatever
/// level it already had.
pub fn parse_level(text: &str) -> Option<LevelFilter> {
    match text.trim().to_ascii_uppercase().as_str() {
        "TRACE" => Some(LevelFilter::Trace),
        "DEBUG" => Some(LevelFilter::Debug),
        "INFO" => Some(LevelFilter::Info),
        "WARNING" | "WARN" => Some(LevelFilter::Warn),
        "ERROR" | "CRITICAL" => Some(LevelFilter::Error),
        _ => None,
    }
}

/// The folder the running binary lives in — the default configuration folder.
pub fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Resolves the configuration folder and reports whether it came from the
/// current-directory fallback.
///
/// Order: an explicit `--config-dir`, then the binary's own folder (the Python
/// rule, and what a deployed installation relies on), then — only when that
/// folder holds no `config.yaml` — the current working directory. The last step
/// exists because `cargo run` puts the executable in `target/debug`, where no
/// configuration lives, while `python main.py` used to find the repository's
/// own `config.yaml`. The fallback is reported so the CLI can say so in the log
/// instead of silently reading a folder the operator did not name.
pub fn resolve_dir(explicit: Option<&Path>) -> (PathBuf, bool) {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    resolve_dir_from(explicit, &exe_dir(), &cwd)
}

fn resolve_dir_from(explicit: Option<&Path>, exe_dir: &Path, cwd: &Path) -> (PathBuf, bool) {
    if let Some(dir) = explicit {
        return (dir.to_path_buf(), false);
    }
    if exe_dir.join(CONFIG_FILE).is_file() {
        return (exe_dir.to_path_buf(), false);
    }
    if cwd != exe_dir && cwd.join(CONFIG_FILE).is_file() {
        return (cwd.to_path_buf(), true);
    }
    // Nothing found: report the binary's folder, so the error message tells the
    // operator where the configuration is expected.
    (exe_dir.to_path_buf(), false)
}

fn read_logging_config(path: &Path, vars: &HashMap<String, String>) -> LoggingConfig {
    // The Python app treats an unreadable or malformed log.yaml as "use the
    // defaults" (`except (FileNotFoundError, yaml.YAMLError)`), never as a fatal
    // error: a typo in the log settings must not stop a running sync. The same
    // applies to an unresolvable placeholder here.
    read_text(path)
        .ok()
        .and_then(|text| serde_norway::from_str::<serde_json::Value>(&text).ok())
        .and_then(|mut document| {
            template::expand_value(&mut document, vars).ok()?;
            Some(document)
        })
        .and_then(|document| serde_json::from_value::<LogFileConfig>(document).ok())
        .and_then(|file| file.logging)
        .unwrap_or_default()
}

fn resolve_log_path(dir: &Path, logging: &LoggingConfig) -> PathBuf {
    let folder = Path::new(&logging.folder);
    let folder = if folder.is_absolute() {
        folder.to_path_buf()
    } else {
        dir.join(folder)
    };
    folder.join(&logging.file_name)
}

/// `dotenv.load_dotenv(<dir>/.env)` — without exporting into the process.
///
/// The returned map is used twice: for `NPM_SOURCE_IDENTITY`/`NPM_SOURCE_SECRET` and for
/// `${VAR}` placeholders in the YAML files, so both always see the same values.
/// The real environment wins over `.env`, as python-dotenv does by default.
fn load_env_map(dir: &Path) -> HashMap<String, String> {
    let mut values: HashMap<String, String> = std::env::vars().collect();

    if let Ok(entries) = dotenvy::from_path_iter(dir.join(ENV_FILE)) {
        for entry in entries.flatten() {
            values.entry(entry.0).or_insert(entry.1);
        }
    }

    values
}

fn credentials_from(vars: &HashMap<String, String>) -> EnvCredentials {
    let value = |name: &str| {
        vars.get(name)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };

    EnvCredentials {
        identity: value(ENV_IDENTITY),
        secret: value(ENV_SECRET),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs() -> (tempfile::TempDir, tempfile::TempDir) {
        (
            tempfile::tempdir().expect("exe dir"),
            tempfile::tempdir().expect("cwd"),
        )
    }

    #[test]
    fn explicit_config_dir_always_wins() {
        let (exe, cwd) = dirs();
        fs::write(
            exe.path().join(CONFIG_FILE),
            "source:\n  url: \"http://x/api\"\n",
        )
        .unwrap();
        fs::write(
            cwd.path().join(CONFIG_FILE),
            "source:\n  url: \"http://y/api\"\n",
        )
        .unwrap();

        let explicit = tempfile::tempdir().expect("explicit");
        let (dir, fallback) = resolve_dir_from(Some(explicit.path()), exe.path(), cwd.path());
        assert_eq!(dir, explicit.path());
        assert!(!fallback);
    }

    #[test]
    fn binary_folder_wins_over_the_current_directory() {
        let (exe, cwd) = dirs();
        fs::write(exe.path().join(CONFIG_FILE), "source: {}\n").unwrap();
        fs::write(cwd.path().join(CONFIG_FILE), "source: {}\n").unwrap();

        let (dir, fallback) = resolve_dir_from(None, exe.path(), cwd.path());
        assert_eq!(
            dir,
            exe.path(),
            "a deployed binary keeps reading its own folder"
        );
        assert!(!fallback);
    }

    #[test]
    fn falls_back_to_the_current_directory_for_cargo_run() {
        let (exe, cwd) = dirs(); // the "exe dir" holds no config.yaml, like target/debug
        fs::write(cwd.path().join(CONFIG_FILE), "source: {}\n").unwrap();

        let (dir, fallback) = resolve_dir_from(None, exe.path(), cwd.path());
        assert_eq!(dir, cwd.path());
        assert!(fallback, "the CLI must be able to report this in the log");
    }

    #[test]
    fn reports_the_binary_folder_when_nothing_is_found() {
        let (exe, cwd) = dirs();
        let (dir, fallback) = resolve_dir_from(None, exe.path(), cwd.path());
        assert_eq!(dir, exe.path());
        assert!(!fallback);
    }

    #[test]
    fn no_fallback_when_both_folders_are_the_same() {
        let dir = tempfile::tempdir().expect("dir");
        let (resolved, fallback) = resolve_dir_from(None, dir.path(), dir.path());
        assert_eq!(resolved, dir.path());
        assert!(!fallback);
    }
}
