//! Entry point. All logic lives in the library so it can be tested without
//! spawning a process; see `nginxy::cli`.

use std::process::ExitCode;

fn main() -> ExitCode {
    nginxy::cli::run(std::env::args_os().skip(1))
}
