//! Command line interface: argument parsing, exit codes and signal setup.
//!
//! The flags, defaults and exit codes are the Python app's; only the help text
//! is rewritten, because `--log-level NONE` means "keep the level from
//! log.yaml" (it does *not* silence logging) and that was never documented.

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
    PossibleValuesParser::new(["DEBUG", "INFO", "WARNING", "ERROR", "CRITICAL", "NONE"])
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
        default_value = "INFO",
        value_name = "LEVEL",
        value_parser = log_level_values(),
        help = "Logging level; NONE keeps the level from log.yaml (default: INFO)"
    )]
    pub log_level: String,

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

    let level = if args.log_level == "NONE" {
        // Deliberate: `NONE` means "do not override log.yaml" in the Python app
        // (PLAN.md §2.5.3), so it must not silence logging here either.
        settings.log_level
    } else {
        settings::parse_level(&args.log_level).unwrap_or(LevelFilter::Info)
    };

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
