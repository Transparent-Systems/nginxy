//! Repository tasks: `cargo xtask release-notes ...` and `cargo xtask schema-check ...`.
//!
//! `release-notes` is called by the build workflows for every `v*` tag, so the
//! notes published with the binaries cannot drift from `RELEASE-NOTES.md`, and a
//! tag that disagrees with the version in `log.yaml` fails the build instead of
//! publishing the wrong notes.
//!
//! `schema-check` reports the field classification of a live NPM instance, so an
//! upgraded NPM that gains a payload field is caught before the next sync.
//!
//! Usage:
//!   cargo xtask release-notes                        # print to stdout
//!   cargo xtask release-notes --output dist/notes.md
//!   cargo xtask release-notes --version 0.1.0        # override log.yaml
//!   cargo xtask release-notes --tag v0.1.0           # verify the tag matches
//!   cargo xtask schema-check --config-dir <PATH>     # classify live NPM fields
//!   cargo xtask schema-check --config-dir <PATH> --save baseline.txt
//!   cargo xtask schema-check --config-dir <PATH> --baseline baseline.txt
//!
//! Exit codes: 0 ok - 1 nothing to classify, or new fields vs a baseline - 2
//! usage, configuration, connection or authentication error.

use std::collections::BTreeSet;
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
        "schema-check" => schema_check(args.collect()),
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

/// `schema-check`: connect to the source NPM in a deployed folder, fetch its
/// proxy hosts, and report how every payload field is classified. With
/// `--save`/`--baseline` it also diffs against a saved field set, so an upgraded
/// NPM that gains a field is caught before the next sync.
fn schema_check(argv: Vec<String>) -> ExitCode {
    let (mut config_dir, mut baseline, mut save) = (None, None, None);
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
            "--config-dir" => config_dir = Some(PathBuf::from(value)),
            "--baseline" => baseline = Some(PathBuf::from(value)),
            "--save" => save = Some(PathBuf::from(value)),
            other => {
                eprintln!("xtask: unknown option {other:?}");
                return ExitCode::from(2);
            }
        }
    }
    let Some(config_dir) = config_dir else {
        eprintln!("xtask: schema-check needs --config-dir <PATH>");
        return ExitCode::from(2);
    };

    let settings = match nginxy::settings::Settings::load(&config_dir) {
        Ok(settings) => settings,
        Err(err) => {
            eprintln!(
                "xtask: cannot load configuration from {}: {err}",
                config_dir.display()
            );
            return ExitCode::from(2);
        }
    };
    let url = settings.source.url.trim().to_string();
    if url.is_empty() {
        eprintln!("xtask: no source url in config.yaml");
        return ExitCode::from(2);
    }

    let mut session = match nginxy::http::Session::new(&url) {
        Ok(session) => session,
        Err(err) => {
            eprintln!("xtask: cannot create HTTP client for {url}: {err}");
            return ExitCode::from(2);
        }
    };
    let auth = nginxy::settings::resolve_auth(settings.source.auth_payload.as_ref(), &settings.env);
    match nginxy::npm::request_token(&session, &auth) {
        Some(token) => session.set_token(Some(token)),
        None => {
            eprintln!("xtask: authentication failed for {url}");
            return ExitCode::from(2);
        }
    }
    let hosts = match nginxy::npm::list_proxy_hosts(&session) {
        Ok(hosts) => hosts,
        Err(err) => {
            eprintln!("xtask: cannot list proxy hosts from {url}: {err:?}");
            return ExitCode::from(2);
        }
    };
    if hosts.is_empty() {
        eprintln!("xtask: no proxy hosts returned by {url}; nothing to classify");
        return ExitCode::from(1);
    }

    let mut keys: BTreeSet<String> = BTreeSet::new();
    for host in &hosts {
        keys.extend(host.rest.keys().cloned());
    }

    print!("{}", render_classification(&keys, &url));

    if let Some(save_path) = &save {
        if let Err(err) = write_key_set(save_path, &keys) {
            eprintln!("xtask: cannot write {}: {err}", save_path.display());
            return ExitCode::from(2);
        }
        eprintln!("xtask: saved baseline to {}", save_path.display());
    }

    let Some(baseline_path) = &baseline else {
        return ExitCode::SUCCESS;
    };
    let baseline_keys = match read_key_set(baseline_path) {
        Ok(keys) => keys,
        Err(err) => {
            eprintln!(
                "xtask: cannot read baseline {}: {err}",
                baseline_path.display()
            );
            return ExitCode::from(2);
        }
    };
    let (added, removed) = diff_key_sets(&keys, &baseline_keys);
    if added.is_empty() && removed.is_empty() {
        eprintln!("xtask: schema unchanged since {}", baseline_path.display());
        return ExitCode::SUCCESS;
    }
    if !added.is_empty() {
        eprintln!(
            "xtask: NEW field(s) since {} - review and classify each:",
            baseline_path.display()
        );
        for key in &added {
            eprintln!("  + {key}");
        }
    }
    if !removed.is_empty() {
        eprintln!(
            "xtask: field(s) no longer present since {}:",
            baseline_path.display()
        );
        for key in &removed {
            eprintln!("  - {key}");
        }
    }
    if added.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// Renders the classification table for a set of observed proxy-host fields.
fn render_classification(keys: &BTreeSet<String>, url: &str) -> String {
    use nginxy::host::FieldClass;

    let mut out = String::new();
    let (mut excluded, mut not_sent, mut propagated) = (0usize, 0usize, 0usize);
    for key in keys {
        let (label, note) = match nginxy::host::classify_field(key) {
            FieldClass::Excluded => {
                excluded += 1;
                ("excluded (server-assigned)", String::new())
            }
            FieldClass::NotSent => {
                not_sent += 1;
                let note = if nginxy::host::REPORTED_AS_NOT_SENT.contains(&key.as_str()) {
                    " (reported when it differs)"
                } else {
                    ""
                };
                ("not sent (per-instance/status)", note.to_string())
            }
            FieldClass::Propagated => {
                propagated += 1;
                ("propagated (compared and sent)", String::new())
            }
        };
        out.push_str(&format!("  {key:<32}{label}{note}\n"));
    }
    out.push('\n');
    out.push_str(&format!(
        "{} proxy-host field(s) from {url}: {excluded} excluded, {not_sent} not sent, {propagated} propagated.\n",
        keys.len()
    ));
    out
}

/// One field name per line: a snapshot of the observed field set.
fn write_key_set(path: &Path, keys: &BTreeSet<String>) -> std::io::Result<()> {
    let mut text = String::new();
    for key in keys {
        text.push_str(key);
        text.push('\n');
    }
    fs::write(path, text)
}

fn read_key_set(path: &Path) -> std::io::Result<BTreeSet<String>> {
    let text = fs::read_to_string(path)?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
}

/// Fields present now but not before, and fields present before but not now.
fn diff_key_sets(
    observed: &BTreeSet<String>,
    baseline: &BTreeSet<String>,
) -> (Vec<String>, Vec<String>) {
    let added: Vec<String> = observed.difference(baseline).cloned().collect();
    let removed: Vec<String> = baseline.difference(observed).cloned().collect();
    (added, removed)
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
        "xtask tasks:\n  release-notes [--tag <TAG>] [--version <X.Y.Z>] [--file <PATH>] [--output <PATH>]\n  schema-check --config-dir <PATH> [--save <PATH>] [--baseline <PATH>]"
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

    #[test]
    fn renders_the_three_classifications() {
        let keys: BTreeSet<String> = ["id", "certificate_id", "locations"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let out = render_classification(&keys, "http://192.0.2.10/api");
        assert!(out.contains("excluded (server-assigned)"), "{out}");
        assert!(out.contains("not sent (per-instance/status)"), "{out}");
        assert!(out.contains("propagated (compared and sent)"), "{out}");
        assert!(
            out.contains("3 proxy-host field(s) from http://192.0.2.10/api"),
            "{out}"
        );
    }

    #[test]
    fn diff_key_sets_reports_added_and_removed() {
        let observed: BTreeSet<String> = ["a", "b"].iter().map(|s| s.to_string()).collect();
        let baseline: BTreeSet<String> = ["b", "c"].iter().map(|s| s.to_string()).collect();
        let (added, removed) = diff_key_sets(&observed, &baseline);
        assert_eq!(added, vec!["a"]);
        assert_eq!(removed, vec!["c"]);
    }
}
