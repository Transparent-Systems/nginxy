//! Command line interface: argument parsing, exit codes and signal setup.
//!
//! The flags, defaults and exit codes are the Python app's, with one deliberate
//! difference: `--log-level` has no default, so `log_level` in `log.yaml`
//! applies unless the flag is given. The Python app's `NONE` spelling for "use
//! log.yaml" was dropped — with the flag optional it had nothing left to mean.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::builder::PossibleValuesParser;
use clap::Parser;
use log::LevelFilter;

use crate::logging;
use crate::settings::{self, Settings, SettingsError};
use crate::stop::Stop;
use crate::sync::{Engine, EXIT_INTERRUPTED};
use crate::version;

/// Exit code for unusable configuration or credentials — what `argparse` and
/// the Python `run()` both return.
pub const EXIT_CONFIG: u8 = 2;

fn log_level_values() -> PossibleValuesParser {
    // The five names Python's `logging` module knows. There is deliberately no
    // `NONE`: "no override" is what omitting `--log-level` does.
    PossibleValuesParser::new(["DEBUG", "INFO", "WARNING", "ERROR", "CRITICAL"])
}

#[derive(Debug, Parser)]
#[command(
    name = "nginxy",
    about = "Synchronise proxy hosts on target Nginx Proxy Manager servers from a source server",
    // `--version` is a plain flag here (as in the Python app) so that it can be
    // read from log.yaml instead of Cargo's version.
    disable_version_flag = true
)]
pub struct Args {
    #[arg(
        short,
        long,
        default_value_t = 30,
        value_name = "SECONDS",
        allow_hyphen_values = true,
        help = "Periodicity of the sync in seconds; 0 syncs once and exits (default: 30)"
    )]
    pub period: i64,

    #[arg(
        short = 'l',
        long,
        value_name = "LEVEL",
        value_parser = log_level_values(),
        help = "Logging level; without it the level from log.yaml applies"
    )]
    pub log_level: Option<String>,

    #[arg(
        long = "dry-run",
        help = "Perform a trial run with no changes made to target servers"
    )]
    pub dry_run: bool,

    #[arg(
        long = "config-dir",
        value_name = "PATH",
        help = "Folder that holds config.yaml, log.yaml and .env (default: the folder of the nginxy binary)"
    )]
    pub config_dir: Option<PathBuf>,

    #[arg(long, help = "Show the version recorded in log.yaml and exit")]
    pub version: bool,
}

/// Runs the CLI and returns the process exit code.
pub fn run<I>(argv: I) -> ExitCode
where
    I: IntoIterator<Item = OsString>,
{
    parse_and_run("nginxy", argv)
}

/// [`run`] with an explicit `argv[0]`, so tests can control the program name.
pub fn parse_and_run<I>(program: &str, argv: I) -> ExitCode
where
    I: IntoIterator<Item = OsString>,
{
    let args = match Args::try_parse_from(std::iter::once(OsString::from(program)).chain(argv)) {
        Ok(args) => args,
        Err(err) => {
            // clap prints --help to stdout and errors to stderr, and uses exit
            // code 0 for the former, 2 for the latter (as argparse does).
            let _ = err.print();
            return ExitCode::from(if err.use_stderr() { EXIT_CONFIG } else { 0 });
        }
    };

    let (dir, used_fallback) = settings::resolve_dir(args.config_dir.as_deref());

    // --version must work without any configuration: it is what a service
    // manager or an upgrade script calls to see what it is about to run.
    if args.version {
        println!("nginxy {}", version::read(&dir));
        return ExitCode::SUCCESS;
    }

    match execute(&args, &dir, used_fallback) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            report(&error);
            ExitCode::from(EXIT_CONFIG)
        }
    }
}

fn execute(args: &Args, dir: &Path, used_fallback: bool) -> Result<u8, SettingsError> {
    let settings = Settings::load(dir)?;

    let level = resolve_level(args.log_level.as_deref(), settings.log_level);

    if let Err(err) = logging::init(
        level,
        &settings.log_path,
        settings.logging.max_bytes,
        settings.logging.max_backup_count,
    ) {
        // An unwritable log file must not stop the sync; stderr still works.
        eprintln!(
            "WARNING: cannot write log file {}: {err}",
            settings.log_path.display()
        );
        logging::init_console_only(level);
    }

    // Say so when the folder was not the one next to the binary: `cargo run`
    // and F5 in an editor land here, and an operator reading the log should not
    // have to guess which configuration was used.
    if used_fallback {
        log::warn!(
            "No config.yaml next to the binary; using {} from the current directory. Pass --config-dir to name another folder.",
            settings.dir.display()
        );
    }

    // Fail immediately instead of retrying every cycle and reporting the 400
    // that NPM returns for an incomplete login payload.
    let missing = settings.missing_credentials();
    if !missing.is_empty() {
        log::error!("No credentials configured for: {}", missing.join(", "));
        log::error!(
            "Create {} with {} and {}, or add auth_payload to that entry in config.yaml.",
            settings.dir.join(settings::ENV_FILE).display(),
            settings::ENV_IDENTITY,
            settings::ENV_SECRET
        );
        return Ok(EXIT_CONFIG);
    }

    let stop = Stop::new();
    install_sigint(&stop);

    let mut engine = match Engine::new(&settings, args.period, args.dry_run, stop) {
        Ok(engine) => engine,
        Err(err) => {
            log::error!("Cannot create the HTTP client: {err}");
            return Ok(EXIT_CONFIG);
        }
    };

    let code = engine.run();
    if code == EXIT_INTERRUPTED {
        log::info!("Sync process interrupted by user. Exiting gracefully.");
    }
    Ok(code)
}

fn install_sigint(stop: &Stop) {
    let stop = stop.clone();
    // Failing here only costs a graceful Ctrl-C, so the failure is not fatal.
    let _ = ctrlc::set_handler(move || stop.request());
}

fn report(error: &SettingsError) {
    match error {
        SettingsError::MissingConfig { path } => {
            // The binary ships without configuration, so this is the first thing
            // a new user hits: explain what is missing instead of a traceback.
            eprintln!("ERROR: configuration file not found: {}", path.display());
            let folder = path
                .parent()
                .map(|parent| parent.display().to_string())
                .unwrap_or_else(|| ".".to_string());
            eprintln!("Copy config.yaml, log.yaml and .env into {folder},");
            eprintln!("or name another folder with --config-dir <PATH>.");
        }
        other => eprintln!("ERROR: {other}"),
    }
}

/// Resolves the logging level: an explicit `--log-level` wins, otherwise the
/// `log_level` from `log.yaml` applies.
///
/// This is the one place the precedence lives, so the tests here and the
/// process-level tests in `tests/cli.rs` pin down the same rule: give the flag
/// and it overrides; leave it out and the file decides.
fn resolve_level(argument: Option<&str>, from_config: LevelFilter) -> LevelFilter {
    match argument {
        None => from_config,
        // Unreachable via clap, which validates the value first; falling back to
        // the configured level rather than a hard-coded one keeps behaviour sane
        // if this is ever called from somewhere else.
        Some(level) => settings::parse_level(level).unwrap_or(from_config),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_the_option_the_configured_level_applies() {
        assert_eq!(resolve_level(None, LevelFilter::Debug), LevelFilter::Debug);
        assert_eq!(resolve_level(None, LevelFilter::Warn), LevelFilter::Warn);
    }

    #[test]
    fn an_explicit_level_wins_over_the_file() {
        assert_eq!(
            resolve_level(Some("ERROR"), LevelFilter::Debug),
            LevelFilter::Error
        );
        assert_eq!(
            resolve_level(Some("DEBUG"), LevelFilter::Info),
            LevelFilter::Debug
        );
    }

    #[test]
    fn none_is_not_a_level() {
        // `NONE` was the Python spelling for "do not override"; with the flag
        // optional it is gone, so a caller passing it falls back to the file.
        assert_eq!(
            resolve_level(Some("NONE"), LevelFilter::Error),
            LevelFilter::Error
        );
    }
}
