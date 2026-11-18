//! Loading `config.yaml`, `log.yaml` and `.env` from one configuration folder.
//!
//! The fixtures below are a realistic deployment: two targets, a third one
//! commented out, the addresses an operator would really use, and the BOM a
//! Windows editor may add. They are the input the binary is handed on a running
//! system, so they are worth testing verbatim rather than in a minimal form.

use std::fs;
use std::path::Path;

use nginxy::settings::{self, Auth, AuthPayload, EnvCredentials, Settings, SettingsError};

const SAMPLE_CONFIG: &str = r#"source:
  url: "http://192.0.2.10:81/api"

targets:
  - name: "NAS2-Nginx"
    url: "http://192.0.2.11:81/api"
  - name: "DEV-Nginx"
    url: "http://192.0.2.12:81/api"
  # - name: "NAS3-Nginx"
  #   url: "http://192.0.2.13:81/api"
  #   auth_payload:
  #     identity: "admin"
  #     secret: "change-me"
"#;

const SAMPLE_LOG: &str = r#"version: 1.1.0
logging:
  file_name: nginxy.log
  folder: logs
  max_bytes: 10485760
  max_backup_count: 3
  # log_level: DEBUG | INFO | WARNING | ERROR | CRITICAL
  log_level: INFO
"#;

fn write(dir: &Path, name: &str, body: &str) {
    fs::write(dir.join(name), body).expect("write fixture");
}

fn load(config: &str, log: &str) -> (tempfile::TempDir, Settings) {
    let dir = tempfile::tempdir().expect("temp dir");
    write(dir.path(), "config.yaml", config);
    write(dir.path(), "log.yaml", log);
    let settings = Settings::load(dir.path()).expect("settings load");
    (dir, settings)
}

#[test]
fn loads_a_deployed_configuration() {
    let (dir, settings) = load(SAMPLE_CONFIG, SAMPLE_LOG);

    assert_eq!(settings.source.url, "http://192.0.2.10:81/api");
    assert_eq!(settings.targets.len(), 2, "the commented target stays out");
    assert_eq!(settings::target_name(&settings.targets[0]), "NAS2-Nginx");
    assert_eq!(settings::target_name(&settings.targets[1]), "DEV-Nginx");
    assert_eq!(settings.targets[1].url, "http://192.0.2.12:81/api");

    // logging defaults and values from log.yaml
    assert_eq!(settings.logging.file_name, "nginxy.log");
    assert_eq!(settings.logging.folder, "logs");
    assert_eq!(settings.logging.max_bytes, 10_485_760);
    assert_eq!(settings.logging.max_backup_count, 3);
    assert_eq!(settings.log_level, log::LevelFilter::Info);
    assert_eq!(
        settings.log_path,
        dir.path().join("logs").join("nginxy.log")
    );
    assert_eq!(settings.dir, dir.path());
}

#[test]
fn accepts_a_byte_order_mark() {
    let dir = tempfile::tempdir().expect("temp dir");
    write(
        dir.path(),
        "config.yaml",
        &format!("\u{feff}{SAMPLE_CONFIG}"),
    );
    write(dir.path(), "log.yaml", &format!("\u{feff}{SAMPLE_LOG}"));

    let settings = Settings::load(dir.path()).expect("settings load with BOM");
    assert_eq!(settings.targets.len(), 2);
    assert_eq!(settings.logging.file_name, "nginxy.log");
}

#[test]
fn missing_config_reports_the_path() {
    let dir = tempfile::tempdir().expect("temp dir");
    let error = Settings::load(dir.path()).expect_err("must fail");
    match error {
        SettingsError::MissingConfig { path } => {
            assert_eq!(path, dir.path().join("config.yaml"));
        }
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn missing_source_url_is_rejected() {
    let dir = tempfile::tempdir().expect("temp dir");
    write(
        dir.path(),
        "config.yaml",
        "targets:\n  - name: \"T\"\n    url: \"http://x/api\"\n",
    );
    let error = Settings::load(dir.path()).expect_err("must fail");
    assert!(matches!(error, SettingsError::NoSourceUrl { .. }));
}

#[test]
fn absent_log_yaml_falls_back_to_defaults() {
    let dir = tempfile::tempdir().expect("temp dir");
    write(dir.path(), "config.yaml", SAMPLE_CONFIG);
    let settings = Settings::load(dir.path()).expect("settings load");
    assert_eq!(settings.logging.file_name, "nginxy.log");
    assert_eq!(settings.logging.max_backup_count, 3);
    assert_eq!(settings.log_level, log::LevelFilter::Info);
}

#[test]
fn absolute_log_folder_is_used_as_given() {
    let elsewhere = tempfile::tempdir().expect("temp dir");
    // A JSON string is also a valid YAML double-quoted scalar, and it escapes
    // the separators. Writing the path raw would break on Windows, where
    // "C:\Users\..." contains YAML escape sequences: log.yaml would not parse,
    // and an unparseable log.yaml deliberately means "use the defaults".
    let folder = serde_json::to_string(&elsewhere.path().display().to_string())
        .expect("quote the log folder");
    let log = format!("logging:\n  folder: {folder}\n  file_name: other.log\n");
    let (_dir, settings) = load(SAMPLE_CONFIG, &log);
    assert_eq!(settings.log_path, elsewhere.path().join("other.log"));
}

#[test]
fn auth_payload_mapping_wins_over_the_environment() {
    let env = EnvCredentials {
        identity: Some("env@example.com".into()),
        secret: Some("envsecret".into()),
    };
    let payload = AuthPayload::Mapping(
        serde_json::json!({"identity": "admin", "secret": "admin-secret"})
            .as_object()
            .unwrap()
            .clone(),
    );
    match settings::resolve_auth(Some(&payload), &env) {
        Auth::Payload(value) => {
            assert_eq!(value["identity"], "admin");
            assert_eq!(value["secret"], "admin-secret");
        }
        other => panic!("unexpected auth: {other:?}"),
    }
}

#[test]
fn auth_payload_json_string_is_accepted() {
    let payload = AuthPayload::Json("{\"identity\": \"a\", \"secret\": \"b\"}".to_string());
    assert_eq!(
        settings::resolve_auth(Some(&payload), &EnvCredentials::default()),
        Auth::Payload(serde_json::json!({"identity": "a", "secret": "b"}))
    );
}

#[test]
fn auth_payload_invalid_json_is_reported() {
    let payload = AuthPayload::Json("{not json".to_string());
    assert_eq!(
        settings::resolve_auth(Some(&payload), &EnvCredentials::default()),
        Auth::InvalidJson
    );
}

#[test]
fn empty_auth_payload_falls_back_to_the_environment() {
    let empty = AuthPayload::Mapping(serde_json::Map::new());
    let env = EnvCredentials {
        identity: Some("env@example.com".into()),
        secret: Some("envsecret".into()),
    };
    assert_eq!(
        settings::resolve_auth(Some(&empty), &env),
        Auth::Payload(serde_json::json!({"identity": "env@example.com", "secret": "envsecret"}))
    );
}

#[test]
fn env_file_supplies_the_credentials() {
    if std::env::var_os("NPM_SOURCE_IDENTITY").is_some() {
        // A developer machine that exports the variable wins over .env, by
        // design; this test is only meaningful when the environment is clean.
        return;
    }

    let dir = tempfile::tempdir().expect("temp dir");
    write(dir.path(), "config.yaml", SAMPLE_CONFIG);
    write(
        dir.path(),
        ".env",
        "NPM_SOURCE_IDENTITY=file@example.com\nNPM_SOURCE_SECRET=filesecret\n",
    );

    let settings = Settings::load(dir.path()).expect("settings load");
    assert_eq!(settings.env.identity.as_deref(), Some("file@example.com"));
    assert_eq!(settings.env.secret.as_deref(), Some("filesecret"));
    assert!(settings.env.is_complete());
}

#[test]
fn missing_credentials_lists_source_then_targets() {
    let (_dir, settings) = load(SAMPLE_CONFIG, SAMPLE_LOG);
    assert_eq!(
        settings.missing_credentials_with(&EnvCredentials::default()),
        vec!["source", "NAS2-Nginx", "DEV-Nginx"]
    );
}

#[test]
fn target_without_name_is_reported_by_url() {
    let (_dir, settings) = load(
        "source:\n  url: \"http://s/api\"\ntargets:\n  - url: \"http://t/api\"\n",
        SAMPLE_LOG,
    );
    assert_eq!(
        settings.missing_credentials_with(&EnvCredentials::default()),
        vec!["source", "http://t/api"]
    );
}

#[test]
fn level_names_are_recognised_regardless_of_case() {
    assert_eq!(
        settings::parse_level("debug"),
        Some(log::LevelFilter::Debug)
    );
    assert_eq!(
        settings::parse_level("WARNING"),
        Some(log::LevelFilter::Warn)
    );
    assert_eq!(
        settings::parse_level("CRITICAL"),
        Some(log::LevelFilter::Error)
    );
    assert_eq!(settings::parse_level("loud"), None);
    // There is no "do not override" level: omitting --log-level is how you opt
    // out, so an old NONE value is rejected like any other unknown name.
    assert_eq!(settings::parse_level("NONE"), None);
    assert_eq!(settings::parse_level("OFF"), None);
}

#[test]
fn placeholders_expand_from_the_env_file() {
    let dir = tempfile::tempdir().expect("temp dir");
    write(
        dir.path(),
        "config.yaml",
        r#"source:
  url: "${NGINXY_TEST_SOURCE_URL}"

targets:
  - name: "FromEnv"
    url: "${NGINXY_TEST_TARGET_URL:-http://127.0.0.1:9/api}"
    auth_payload:
      identity: "${NGINXY_TEST_IDENTITY}"
      secret: "${NGINXY_TEST_SECRET}"
"#,
    );
    write(
        dir.path(),
        ".env",
        "NGINXY_TEST_SOURCE_URL=http://192.0.2.10:81/api\n\
         NGINXY_TEST_IDENTITY=admin\n\
         NGINXY_TEST_SECRET=admin-secret\n",
    );
    write(
        dir.path(),
        "log.yaml",
        "version: 9.9.9\nlogging:\n  folder: \"${NGINXY_TEST_LOG_DIR:-logs}\"\n",
    );

    let settings = Settings::load(dir.path()).expect("settings load");
    assert_eq!(settings.source.url, "http://192.0.2.10:81/api");
    // No target URL in .env, so the inline default applies.
    assert_eq!(settings.targets[0].url, "http://127.0.0.1:9/api");
    assert_eq!(settings.logging.folder, "logs");

    // The target's auth_payload was filled from env, so only the source (which
    // has no auth_payload) is reported missing when the environment is empty.
    assert_eq!(
        settings.missing_credentials_with(&EnvCredentials::default()),
        vec!["source"]
    );
}

#[test]
fn unresolved_placeholder_names_the_file_and_variable() {
    let dir = tempfile::tempdir().expect("temp dir");
    write(
        dir.path(),
        "config.yaml",
        "source:\n  url: \"${NGINXY_TEST_UNSET_VARIABLE}\"\n",
    );

    let error = Settings::load(dir.path()).expect_err("must fail");
    match error {
        SettingsError::Template { path, message } => {
            assert!(path.ends_with("config.yaml"));
            assert!(message.contains("NGINXY_TEST_UNSET_VARIABLE"), "{message}");
            assert!(message.contains("source.url"), "{message}");
        }
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn log_yaml_placeholders_never_break_the_defaults() {
    let dir = tempfile::tempdir().expect("temp dir");
    write(dir.path(), "config.yaml", SAMPLE_CONFIG);
    // Unresolvable placeholder in log.yaml must not be fatal, exactly like a
    // malformed log.yaml.
    write(
        dir.path(),
        "log.yaml",
        "logging:\n  folder: \"${NGINXY_TEST_ALSO_UNSET}\"\n",
    );

    let settings = Settings::load(dir.path()).expect("settings load");
    assert_eq!(settings.logging.folder, "logs", "defaults are used instead");
}

#[test]
fn placeholders_inside_comments_are_ignored() {
    // The shipped config.yaml documents the syntax in a comment block; those
    // `${VAR}` spellings must not be resolved (the YAML parser drops them).
    let dir = tempfile::tempdir().expect("temp dir");
    write(
        dir.path(),
        "config.yaml",
        "# Supported: ${VAR}, ${VAR:-default}, ${VAR:?message}, $$ for a literal $\n\
         source:\n  url: \"http://127.0.0.1:9/api\"\n\
         # another ${DOC_ONLY} placeholder\n",
    );

    let settings = Settings::load(dir.path()).expect("comments are not expanded");
    assert_eq!(settings.source.url, "http://127.0.0.1:9/api");
}

#[test]
fn credentials_come_only_from_the_source_names() {
    if std::env::var_os("NPM_SOURCE_IDENTITY").is_some() {
        return;
    }

    let dir = tempfile::tempdir().expect("temp dir");
    write(dir.path(), "config.yaml", SAMPLE_CONFIG);
    write(
        dir.path(),
        ".env",
        "NPM_SOURCE_IDENTITY=source@example.com\nNPM_SOURCE_SECRET=sourcesecret\n",
    );

    let settings = Settings::load(dir.path()).expect("settings load");
    assert_eq!(settings.env.identity.as_deref(), Some("source@example.com"));
    assert_eq!(settings.env.secret.as_deref(), Some("sourcesecret"));
    assert!(settings.missing_credentials_with(&settings.env).is_empty());
}
