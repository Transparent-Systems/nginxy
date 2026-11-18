//! Process-level behaviour: exit codes, `--version` without configuration, and
//! the fact that the log file really lands in `--config-dir`.

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;

fn nginxy() -> Command {
    Command::cargo_bin("nginxy").expect("binary")
}

fn write(dir: &Path, name: &str, body: &str) {
    fs::write(dir.join(name), body).expect("write fixture");
}

/// A configuration folder pointing at a port nothing listens on (9/discard),
/// which is the same trick the release smoke tests use.
fn dead_end_config(dir: &Path) {
    write(
        dir,
        "config.yaml",
        "source:\n  url: \"http://127.0.0.1:9/api\"\ntargets:\n  - name: \"Smoke\"\n    url: \"http://127.0.0.1:9/api\"\n",
    );
    write(
        dir,
        "log.yaml",
        "version: 9.9.9\nlogging:\n  file_name: nginxy.log\n  folder: logs\n  log_level: INFO\n",
    );
    write(
        dir,
        ".env",
        "NPM_SOURCE_IDENTITY=smoke\nNPM_SOURCE_SECRET=smoke\n",
    );
}

#[test]
fn version_works_without_any_configuration() {
    let dir = tempfile::tempdir().expect("temp dir");
    nginxy()
        .args(["--version", "--config-dir"])
        .arg(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::starts_with("nginxy "));
}

#[test]
fn version_is_read_from_log_yaml() {
    let dir = tempfile::tempdir().expect("temp dir");
    write(dir.path(), "log.yaml", "version: 1.1.0\n");
    nginxy()
        .args(["--version", "--config-dir"])
        .arg(dir.path())
        .assert()
        .success()
        .stdout("nginxy 1.1.0\n");
}

#[test]
fn help_lists_the_python_arguments() {
    nginxy()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--period"))
        .stdout(predicate::str::contains("--log-level"))
        .stdout(predicate::str::contains("--dry-run"))
        .stdout(predicate::str::contains("--config-dir"))
        .stdout(predicate::str::contains("--version"));
}

#[test]
fn missing_configuration_exits_with_two_and_explains() {
    let dir = tempfile::tempdir().expect("temp dir");
    nginxy()
        .args(["-p", "0", "--config-dir"])
        .arg(dir.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "ERROR: configuration file not found",
        ))
        .stderr(predicate::str::contains(
            "or name another folder with --config-dir <PATH>.",
        ));
}

#[test]
fn missing_credentials_exit_with_two() {
    let dir = tempfile::tempdir().expect("temp dir");
    write(
        dir.path(),
        "config.yaml",
        "source:\n  url: \"http://127.0.0.1:9/api\"\ntargets:\n  - name: \"NAS2-Nginx\"\n    url: \"http://127.0.0.1:9/api\"\n",
    );
    write(dir.path(), "log.yaml", "version: 9.9.9\n");

    nginxy()
        .args(["-p", "0", "--config-dir"])
        .arg(dir.path())
        .env_remove("NPM_SOURCE_IDENTITY")
        .env_remove("NPM_SOURCE_SECRET")
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "No credentials configured for: source, NAS2-Nginx",
        ));
}

#[test]
fn unknown_log_level_is_rejected_with_exit_code_two() {
    let dir = tempfile::tempdir().expect("temp dir");
    dead_end_config(dir.path());
    nginxy()
        .args(["-l", "LOUD", "--config-dir"])
        .arg(dir.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains("invalid value"));
}

#[test]
fn one_shot_sync_writes_the_log_into_the_config_folder() {
    let dir = tempfile::tempdir().expect("temp dir");
    dead_end_config(dir.path());

    nginxy()
        .args(["-p", "0", "--config-dir"])
        .arg(dir.path())
        .assert()
        .success();

    let log = fs::read_to_string(dir.path().join("logs").join("nginxy.log"))
        .expect("log file written into --config-dir");
    assert!(
        log.contains("Starting Sync: 1 targets configured."),
        "{log}"
    );
    assert!(log.contains("Source session not authenticated."), "{log}");
    // Python's line format: date, comma, milliseconds, level, message.
    let first = log.lines().next().expect("at least one line");
    assert!(
        first.contains(" - INFO - Starting Sync"),
        "unexpected format: {first}"
    );
}

#[test]
fn dry_run_is_reported_in_the_log() {
    let dir = tempfile::tempdir().expect("temp dir");
    dead_end_config(dir.path());

    nginxy()
        .args(["-p", "0", "--dry-run", "--config-dir"])
        .arg(dir.path())
        .assert()
        .success();

    let log = fs::read_to_string(dir.path().join("logs").join("nginxy.log")).expect("log");
    assert!(
        log.contains("Starting Sync (DRY RUN MODE): 1 targets configured."),
        "{log}"
    );
}

/// Ctrl-C must end the run promptly even in the middle of a long period, with
/// exit code 130 and the shutdown message (PLAN.md §2.1).
#[cfg(unix)]
#[test]
fn ctrl_c_exits_with_130_during_a_long_period() {
    use std::process::{Command as Process, Stdio};
    use std::time::{Duration, Instant};

    let dir = tempfile::tempdir().expect("temp dir");
    dead_end_config(dir.path());

    let mut child = Process::new(assert_cmd::cargo::cargo_bin("nginxy"))
        .args(["-p", "300", "--config-dir"])
        .arg(dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn nginxy");

    // Give it time to log in (and fail), then interrupt the 300 s sleep.
    std::thread::sleep(Duration::from_millis(1500));

    let started = Instant::now();
    let kill = Process::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .expect("send SIGINT");
    assert!(kill.success(), "kill -INT failed");

    let status = child.wait().expect("wait for nginxy");
    assert_eq!(status.code(), Some(130));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "shutdown took {:?}",
        started.elapsed()
    );

    let log = fs::read_to_string(dir.path().join("logs").join("nginxy.log")).expect("log");
    assert!(
        log.contains("Sync process interrupted by user. Exiting gracefully."),
        "{log}"
    );
}

/// The `cargo run` / editor-F5 case: the executable sits in `target/debug`, the
/// configuration in the repository, and no `--config-dir` is given.
#[test]
fn run_without_config_dir_uses_the_current_directory() {
    let dir = tempfile::tempdir().expect("temp dir");
    dead_end_config(dir.path());

    nginxy()
        .args(["-p", "0"])
        .current_dir(dir.path())
        .assert()
        .success();

    let log = fs::read_to_string(dir.path().join("logs").join("nginxy.log")).expect("log");
    assert!(
        log.contains("No config.yaml next to the binary; using"),
        "the fallback must be visible in the log: {log}"
    );
    assert!(
        log.contains("Starting Sync: 1 targets configured."),
        "{log}"
    );
}

/// An explicit `--config-dir` must never fall back anywhere.
#[test]
fn explicit_config_dir_is_not_affected_by_the_current_directory() {
    let config = tempfile::tempdir().expect("config dir");
    dead_end_config(config.path());
    let elsewhere = tempfile::tempdir().expect("cwd");
    dead_end_config(elsewhere.path());

    nginxy()
        .args(["-p", "0", "--config-dir"])
        .arg(config.path())
        .current_dir(elsewhere.path())
        .assert()
        .success();

    let log = fs::read_to_string(config.path().join("logs").join("nginxy.log")).expect("log");
    assert!(!log.contains("from the current directory"), "{log}");
    assert!(
        !elsewhere.path().join("logs").join("nginxy.log").exists(),
        "the current directory must not be used"
    );
}

/// `log_level` in `log.yaml` must be honoured without any command-line flag
/// (PLAN.md §2.6). `log_level: ERROR` hides the INFO lines but keeps the ERROR
/// ones.
#[test]
fn log_level_from_log_yaml_applies_without_the_option() {
    let dir = tempfile::tempdir().expect("temp dir");
    dead_end_config(dir.path());
    write(
        dir.path(),
        "log.yaml",
        "version: 9.9.9\nlogging:\n  file_name: nginxy.log\n  folder: logs\n  log_level: ERROR\n",
    );

    nginxy()
        .args(["-p", "0", "--config-dir"])
        .arg(dir.path())
        .assert()
        .success();

    let log = fs::read_to_string(dir.path().join("logs").join("nginxy.log")).expect("log");
    assert!(
        !log.contains("Starting Sync"),
        "an INFO line must be filtered out at ERROR level: {log}"
    );
    assert!(
        log.contains(" - ERROR - "),
        "errors are still logged: {log}"
    );
}

/// An explicit `--log-level` overrides the file.
#[test]
fn log_level_option_overrides_log_yaml() {
    let dir = tempfile::tempdir().expect("temp dir");
    dead_end_config(dir.path());
    write(
        dir.path(),
        "log.yaml",
        "version: 9.9.9\nlogging:\n  file_name: nginxy.log\n  folder: logs\n  log_level: ERROR\n",
    );

    nginxy()
        .args(["-l", "INFO", "-p", "0", "--config-dir"])
        .arg(dir.path())
        .assert()
        .success();

    let log = fs::read_to_string(dir.path().join("logs").join("nginxy.log")).expect("log");
    assert!(
        log.contains("Starting Sync"),
        "INFO must win over log.yaml here: {log}"
    );
}

/// `NONE` is gone: it is not a log level, and "use log.yaml" is now what
/// omitting `-l` does. The CLI rejects it like any other unknown value.
#[test]
fn none_is_rejected_as_a_level() {
    let dir = tempfile::tempdir().expect("temp dir");
    dead_end_config(dir.path());

    nginxy()
        .args(["-l", "NONE", "-p", "0", "--config-dir"])
        .arg(dir.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains("invalid value"));
}
