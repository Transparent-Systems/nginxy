//! Repository tasks: `cargo xtask release-notes …`
//!
//! The build workflows call it for every `v*` tag, so the notes published with
//! the binaries cannot drift from `RELEASE-NOTES.md`, and a tag that disagrees
//! with the version in `log.yaml` fails the build instead of publishing the
//! wrong notes.
//!
//! Usage:
//!   cargo xtask release-notes                        # print to stdout
//!   cargo xtask release-notes --output dist/notes.md
//!   cargo xtask release-notes --version 0.1.0        # override log.yaml
//!   cargo xtask release-notes --tag v0.1.0           # verify the tag matches
//!
//! Exit codes: 0 extracted · 1 no section for that version · 2 tag/version
//! mismatch or unreadable file.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const NOTES_FILE: &str = "RELEASE-NOTES.md";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(task) = args.next() else {
        usage();
        return ExitCode::from(2);
    };

    match task.as_str() {
        "release-notes" => release_notes(args.collect()),
        "-h" | "--help" | "help" => {
            usage();
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("xtask: unknown task {other:?}");
            usage();
            ExitCode::from(2)
        }
    }
}

#[derive(Default)]
struct Options {
    tag: Option<String>,
    version: Option<String>,
    output: Option<PathBuf>,
    file: Option<PathBuf>,
}

fn release_notes(argv: Vec<String>) -> ExitCode {
    let mut options = Options::default();
    let mut argv = argv.into_iter();

    while let Some(flag) = argv.next() {
        let value = match argv.next() {
            Some(value) => value,
            None => {
                eprintln!("xtask: {flag} needs a value");
                return ExitCode::from(2);
            }
        };
        match flag.as_str() {
            "--tag" => options.tag = Some(value),
            "--version" => options.version = Some(value),
            "--output" => options.output = Some(PathBuf::from(value)),
            "--file" => options.file = Some(PathBuf::from(value)),
            other => {
                eprintln!("xtask: unknown option {other:?}");
                return ExitCode::from(2);
            }
        }
    }

    // `version::read(".")` prefers ./log.yaml, the single place the project
    // records its version.
    let config_version = nginxy::version::read(Path::new("."));
    let version = options.version.unwrap_or_else(|| config_version.clone());

    if let Some(tag) = &options.tag {
        let normalized = tag.trim().trim_start_matches('v');
        if normalized != version {
            eprintln!("xtask: tag {tag} does not match version {version} in log.yaml");
            return ExitCode::from(2);
        }
    }

    let file = options.file.unwrap_or_else(|| PathBuf::from(NOTES_FILE));
    let text = match fs::read_to_string(&file) {
        Ok(text) => text,
        Err(err) => {
            eprintln!("xtask: cannot read {}: {err}", file.display());
            return ExitCode::from(2);
        }
    };

    let Some(section) = extract_section(&text, &version) else {
        eprintln!("xtask: no section for {version} in {}", file.display());
        return ExitCode::from(1);
    };

    match options.output {
        Some(path) => {
            if let Some(parent) = path.parent() {
                if let Err(err) = fs::create_dir_all(parent) {
                    eprintln!("xtask: cannot create {}: {err}", parent.display());
                    return ExitCode::from(2);
                }
            }
            if let Err(err) = fs::write(&path, &section) {
                eprintln!("xtask: cannot write {}: {err}", path.display());
                return ExitCode::from(2);
            }
        }
        None => print!("{section}"),
    }

    ExitCode::SUCCESS
}

/// Returns the body of the `## <version>` section, heading excluded.
///
/// A section starts at its level-2 heading and ends at the next one, so
/// subsections stay part of the body.
fn extract_section(text: &str, version: &str) -> Option<String> {
    let mut body = String::new();
    let mut inside = false;

    for line in text.lines() {
        if line.starts_with("## ") {
            if inside {
                break;
            }
            if heading_version(line).as_deref() == Some(version) {
                inside = true;
            }
            continue;
        }
        if inside {
            body.push_str(line);
            body.push('\n');
        }
    }

    if !inside {
        return None;
    }

    let trimmed = body.trim_matches('\n');
    if trimmed.is_empty() {
        Some(String::new())
    } else {
        Some(format!("{trimmed}\n"))
    }
}

/// `## v1.0.0 - first release`, `## 1.0.0`, `## [1.0.0] - 2026-05-09`
fn heading_version(line: &str) -> Option<String> {
    let rest = line.trim_start_matches('#').trim();
    let token = match rest.strip_prefix('[') {
        Some(bracketed) => bracketed.split(']').next()?.trim(),
        None => rest.split_whitespace().next()?,
    };
    Some(token.trim_start_matches('v').to_string())
}

fn usage() {
    println!(
        "xtask tasks:\n  release-notes [--tag <TAG>] [--version <X.Y.Z>] [--file <PATH>] [--output <PATH>]"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTES: &str = "# Release Notes\n\n## 0.2.0 - next\n\nAdded a thing.\n\n### Detail\n\nMore.\n\n## 0.1.0 - first\n\nInitial.\n";

    #[test]
    fn extracts_the_matching_section_only() {
        let section = extract_section(NOTES, "0.2.0").expect("section");
        assert_eq!(section, "Added a thing.\n\n### Detail\n\nMore.\n");
    }

    #[test]
    fn strips_leading_v_and_brackets() {
        assert_eq!(
            heading_version("## v1.1.0 - add Linux binary").as_deref(),
            Some("1.1.0")
        );
        assert_eq!(
            heading_version("## [1.0.0] - 2026-05-09").as_deref(),
            Some("1.0.0")
        );
        assert_eq!(heading_version("## 1.0.0").as_deref(), Some("1.0.0"));
    }

    #[test]
    fn missing_version_returns_none() {
        assert!(extract_section(NOTES, "9.9.9").is_none());
    }
}
