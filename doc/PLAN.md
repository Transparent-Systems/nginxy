# nginxy - design and behaviour

Status: implemented. Section 2 is the behavioural contract the code and tests are held
to, Section 6 lists the evidence, Section 10 records the decisions and why each was taken.

Repository: `Transparent-Systems/nginxy`
Toolchain: stable Rust; the floor is `rust-version = "1.85"` in `Cargo.toml`.

---

## 1. Purpose and scope

**Purpose.** Keep one Nginx Proxy Manager instance in step with another: read the
proxy hosts from a *source* instance, then make every *target* instance match it.

Distribution is one native binary per platform - no interpreter, no service
manager, no database. On Linux it is a static musl executable; on Windows it is a
single unsigned `.exe` that needs nothing installed.

**In scope**

- One source and one or more targets, all reached over the NPM HTTP API
  (`/api/tokens`, `/api/nginx/proxy-hosts`).
- A polling loop: one cycle every `--period` seconds, `-p 0` runs a single cycle
  and exits, `--dry-run` reports what would change and writes nothing.
- Three configuration files in one folder: `config.yaml`, `log.yaml`, `.env`.

**Out of scope**

- No scheduling UI, metrics endpoint or configuration hot-reload.
- No GUI, database or service-management code: systemd, Task Scheduler or NSSM
  start the binary and restart it if needed.
- No change to the NPM API surface used, and no support for other proxies.

Everything operator-visible in Section 2 is contractual: scripts, CI and service units
depend on it, so changing it is a recorded decision (Section 10), never a side effect of
a refactor.

---

## 2. Behavioural contract

### 2.1 Command line

| Flag | Type | Default | Notes |
| --- | --- | --- | --- |
| `-p`, `--period <SECONDS>` | integer | `30` | `0` or less runs one cycle and exits |
| `-l`, `--log-level <LEVEL>` | `DEBUG\|INFO\|WARNING\|ERROR\|CRITICAL` | unset -> `log.yaml` | An explicit value overrides `log_level`; there is no `NONE` value (Section 2.6) |
| `--dry-run` | flag | off | No `POST`/`PUT`/`DELETE`; the source-change marker is not consumed |
| `--config-dir <PATH>` | path | binary's folder | Always wins over the folder next to the binary |
| `--version` | flag | - | Must work with **no** configuration present; prints the binary's own version, never the config |
| `-h`, `--help` | flag | - | Exit 0 |

Exit codes: `0` success - `2` unusable configuration, credentials or arguments -
`130` interrupted by Ctrl-C.

### 2.2 Files, names and formats

- **Config folder.** `--config-dir` wins. Otherwise the folder holding the
  executable, but only if it contains `config.yaml`; when it does not, the
  current working directory is used and a `WARNING` names the folder that was
  chosen. That fallback is what makes `cargo run` and an editor's Run button work
  from the repository root without passing anything.
- **Encoding.** `config.yaml`, `log.yaml` and `.env` are read as UTF-8 with an
  optional BOM; the BOM is stripped before parsing because the YAML parser would
  otherwise reject it.
- **`config.yaml`.** `source.url` plus optional `source.auth_payload`, and
  `targets`: a list of `{ name?, url, auth_payload? }`. A target without `name`
  is reported by its URL.
- **`log.yaml`.** `version:` (what `--version` prints) and
  `logging.{file_name, folder, max_bytes, max_backup_count, log_level}`. A
  missing or unparseable `log.yaml` is never fatal: the defaults apply, because a
  typo in the log settings must not stop a running sync.
- **`.env`.** `NPM_SOURCE_IDENTITY` and `NPM_SOURCE_SECRET`, used by every
  endpoint that has no `auth_payload` of its own. The process environment wins
  over the file, so a container or service unit can supply them instead.
- **Placeholders.** `config.yaml` and `log.yaml` values may use `${VAR}`,
  `${VAR:-default}`, `${VAR:?message}` and `$$` for a literal `$`. Values come
  from the process environment and then `.env`. Only parsed *values* are
  expanded, so documentation comments stay inert; an unset `${VAR}` without a
  default stops startup naming the file, the setting and the variable.
- **Log file.** `<config-dir>/<logging.folder>/<logging.file_name>`, default
  `logs/nginxy.log`. A relative `folder` is resolved against the config folder,
  and the folder is created when absent. If the file cannot be opened, a
  `WARNING` goes to stderr and logging continues on the console only - an
  unwritable log must not stop a sync.
- **Rotation.** Before a write that would exceed `max_bytes` (default
  10 485 760), rename `x.log` -> `x.log.1` ... shifting older copies up to
  `max_backup_count` (default 3) and deleting anything beyond. `max_bytes: 0`
  disables rotation. Records are UTF-8, flushed per line.

### 2.3 Log lines

`YYYY-MM-DD HH:MM:SS,mmm - LEVEL - message` - local time, milliseconds after a
comma, one record per line, written to the log file **and** to stderr:

```
2026-11-18 08:12:33,123 - INFO - Starting Sync: 2 targets configured.
```

Level names are `DEBUG`, `INFO`, `WARNING`, `ERROR`, `CRITICAL`. Messages other
than these are informative; these are the ones operators and scripts match on:

- `Starting Sync: N targets configured.` - with ` (DRY RUN MODE)` appended
- `No changes in source and all targets are in sync.`
- `Retrying sync for N unsynced target(s).`
- `Host ["a.com", "b.com"] is already up to date on target.`
- `[DRY RUN] Would create host: ...` (likewise `update` and `delete`)
- `Sync process interrupted by user. Exiting gracefully.`

Domain lists are rendered the Rust way - `["a.com", "b.com"]` - because `{:?}` on
a `Vec<String>` keeps the code plain; the information is the same (Section 10-D3).

### 2.4 What a sync does

```
loop:
  phase 0: every 12 h drop all Authorization headers   (tokens are valid 24 h)
  phase 1: GET source /nginx/proxy-hosts               -> source_hosts
  phase 2: has the source changed since the last marker?
             yes -> mark every target as needing a sync (not in --dry-run)
  phase 3: for each target that needs a sync:
             GET target /nginx/proxy-hosts
             key every host by its sorted domain_names
             pass A: delete target keys the source does not have
             pass B: per source host - payload equal -> log "up to date"
                                       differs      -> PUT
                                       missing      -> POST
  sleep(period), or exit when period <= 0
```

Pass A must run before pass B: a host whose domain set changed looks like a
delete plus a create, and the create would collide with the old name.

A target whose pass fails stays queued and is retried every cycle until one pass
succeeds, so a transient 502 delays a sync instead of losing it.

### 2.5 Change detection and payload filtering

"Changed" means the *payload* changed. The fingerprint ignores `EXCLUDED_KEYS`
(`id`, `owner_user_id`, `created_on`, `modified_on`, `user`) and `NOT_SENT`
(`certificate_id`, `access_list_id`, `meta`), so saving a host on the source
without editing anything does not re-sync every target.

The comparison is a deny-list rather than a list of interesting fields: a field
added by a future NPM release is propagated by construction. The unit test
`every_field_is_classified` fails when a field is neither propagated nor
excluded, so a new key cannot be dropped silently; `certificate_id` and
`access_list_id` are also logged as not sent, because an operator could
reasonably expect them to travel. Create and update send the same filtered
object that was compared.

### 2.6 Rules worth knowing

1. **`log_level` in `log.yaml` applies on its own.** `--log-level` has no
   default, so it overrides only when given. `NONE` is not a level: omitting the
   flag is how you opt out, and any unknown name is rejected with exit 2.
2. **Credentials resolve per endpoint.** `auth_payload` - a mapping or a JSON
   string - wins over `.env`; an empty mapping falls through to `.env`; a
   `auth_payload` string that is not valid JSON is reported as such. Endpoints
   with no usable credentials are listed (source first, then targets) and the
   process exits 2 before the first request.
3. **`--dry-run` changes nothing**, including the change-detection marker, so the
   next cycle still reports the same pending work.
4. **A failed write keeps the target queued** and the pass reports the failure,
   instead of calling the target synchronised and waiting for the next change on
   the source.
5. **Ctrl-C stops immediately**, even mid-period, exiting 130 with the graceful
   message above.

---

## 3. Dependencies

| Concern | Crate | Why this one |
| --- | --- | --- |
| CLI | `clap` 4 (`derive`) | De-facto standard; gives `-h`, value validation and exit codes for free. `value_parser` enforces the five level names |
| HTTP | `reqwest` 0.13, features `blocking`, `json`, `rustls` | The program is a sleep-loop, not a server: async buys nothing and costs a runtime. `rustls` avoids OpenSSL (and its MSVC pain). In 0.13 the feature is named `rustls`; the backend is aws-lc-rs, which is why the Linux build needs `cmake` |
| JSON | `serde`, `serde_json` | API payloads. `serde_json::Value` keeps integers and floats apart, so `forward_port: 80` does not become `80.0` |
| YAML | `serde_norway` | Maintained fork of the abandoned `serde_yaml` 0.9, API-identical, so switching again is a one-line change |
| `.env` | `dotenvy` | Loads without exporting into the process, and the real environment wins - which is what a service unit or container expects |
| Logging | `log` (feature `std`) + a hand-written `Log` impl (~80 LOC) | `env_logger`/`tracing-subscriber` cannot produce the line format or the rotation described in Sections 2.2 and 2.3 without more code than writing it. `std` must be enabled explicitly for `set_boxed_logger` |
| Timestamps | `chrono` (`clock`) | Local time with millisecond formatting |
| Ctrl-C | `ctrlc` | Portable interrupt handler, including Windows |
| Errors | `thiserror` | Explicit error types; no `anyhow` in library APIs |
| Tests | `mockito`, `tempfile`, `assert_cmd`, `predicates` | `mockito` matches the blocking client; `assert_cmd` drives the real binary |

Deliberately avoided: `tokio`/`axum` (no server), `async-std`, `openssl`,
`serde_yaml` (EOL), `lazy_static` (use `OnceLock`), `error-chain`.

Minimum supported Rust: **1.85** (`rust-version` in `Cargo.toml`).

---

## 4. Project layout

```
nginxy/
├── Cargo.toml                 # package nginxy: lib, bin, and bin/xtask
├── Cargo.lock                 # committed (this is an application)
├── rust-toolchain.toml        # channel = "stable", components fmt + clippy
├── .cargo/config.toml         # `cargo xtask` alias; static CRT on Windows
├── .gitignore                 # /target, .env, /logs/, /dist/
├── README.md                  # install, configure, run
├── RELEASE-NOTES.md           # one `## <version>` section per release
├── LICENSE                    # MIT
├── doc/PLAN.md                # this document
├── config.yaml                # sample configuration
├── log.yaml                   # sample logging configuration and version
├── src/
│   ├── main.rs                # entry point: parse, run, exit code
│   ├── lib.rs                 # module map and re-exports
│   ├── cli.rs                 # clap `Args`, level resolution, SIGINT, exit codes
│   ├── settings.rs            # config folder, YAML models, .env, BOM, auth
│   ├── logging.rs             # Log impl, record formatting, rotating file
│   ├── http.rs                # blocking client + bearer token per endpoint
│   ├── npm.rs                 # token, list, create, update, delete
│   ├── host.rs                # ProxyHost, payload filtering, domain keys
│   ├── sync.rs                # Engine: phases 0-3, retry set, dry run
│   ├── stop.rs                # interruptible sleep
│   ├── version.rs             # version resolution for --version
│   └── bin/xtask.rs           # `cargo xtask release-notes`
├── tests/
│   ├── settings.rs            # configuration loading end to end
│   ├── npm_api.rs             # mock NPM: verbs, paths, payloads, ordering
│   └── cli.rs                 # process level: --version, exit codes, signals
└── .github/workflows/
    ├── ci.yml                 # fmt, clippy -D warnings, test on Linux + Windows
    ├── build-linux.yml        # musl build, smoke test, sha256, release
    └── build-windows.yml      # MSVC build, smoke test, sha256, release
```

One package with a library and a thin binary: the logic is testable without a
process, and a second front end (service wrapper, GUI) would not touch `src/`.

---

## 5. Module responsibilities

| Module | Owns |
| --- | --- |
| `main` | `ExitCode` from the CLI, nothing else |
| `cli` | Argument parsing, level precedence, credential check, signal install, error reporting |
| `settings` | Config folder resolution, `config.yaml` + `log.yaml` + `.env`, BOM strip, placeholder expansion, auth resolution, log path |
| `template` | `${VAR}`, `${VAR:-default}`, `${VAR:?message}` and `$$` over a parsed YAML value |
| `logging` | The `log::Log` implementation, record format, rotate-then-write file output, stderr output |
| `http` | One `Session` per endpoint URL (client + bearer token), timeout, failure description |
| `npm` | The four API calls and their log lines |
| `host` | `ProxyHost` model, payload filtering, domain-set keys, payload comparison |
| `sync` | The loop: token refresh, source fetch, change marker, per-target passes, retry set, dry run |
| `stop` | `AtomicBool` plus interruptible sleep, so Ctrl-C is immediate |
| `version` | `log.yaml` -> build-time version -> crate version -> `unknown` |
| `bin/xtask` | Release-notes extraction and the tag/version check |

---

## 6. Verification

`cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings` and
`cargo test --all-targets` must be clean on Linux and Windows; CI enforces all
three on every push.

| Suite | Tests | What it pins down |
| --- | --- | --- |
| `src/**` unit tests | 55 | Payload filtering and field classification, domain keys, record formatting, rotation, placeholder grammar, version fallbacks, level parsing |
| `src/bin/xtask.rs` | 3 | Section extraction, tag/version mismatch, missing section |
| `tests/settings.rs` | 19 | A deployed configuration loads, BOM, missing config, absolute log folder, auth precedence, `.env`, placeholders, missing-credential list |
| `tests/npm_api.rs` | 12 | Verbs, paths, bearer header, payload shape, delete-before-create, dry-run silence |
| `tests/cli.rs` | 14 | `--version`/`--help` without config, exit 2 cases, log written into `--config-dir`, Ctrl-C exits 130 |
| **total** | **103** | |

Release builds are exercised by the same smoke test CI runs: `--version`,
`--help`, then a real one-shot run with `--config-dir` against port 9 (which
refuses the connection) asserting that `Starting Sync` reached the log file. That
is what catches a broken bundle or a missing runtime dependency, which is why the
Windows job also inspects the exe (Section 8.2).

---

## 7. Testing strategy

| Level | Tool | What it locks down |
| --- | --- | --- |
| Unit | `#[test]` in the module | Payload filtering and classification, domain-key sorting and duplicates, rotation and record formatting, placeholder grammar, version fallbacks |
| Configuration | `tempfile` + real YAML files | A deployed folder loads, BOM accepted, placeholders expanded, auth precedence, error messages name the file and setting |
| HTTP contract | `mockito` | Paths, verbs, `Authorization` header, payload shape, delete-before-create, dry-run silence |
| Process | `assert_cmd`, `predicates`, `tempfile` | `--version`/`--help` with no configuration, exit 2 cases, the log file lands in `--config-dir` |
| Signals | `Child` + `libc::kill(pid, SIGINT)` (`#[cfg(unix)]`) | Exit 130, graceful message, shutdown well under 200 ms |
| Cross-OS | CI matrix `ubuntu-latest`, `windows-latest` | Path handling, stderr, a sha256 file with LF endings and no BOM |

Deliberately not tested: live NPM servers (environment-specific) and the 12-hour
token refresh (the interval is injected, so tests use milliseconds).

---

## 8. Build and release

### 8.1 Linux - static musl

```bash
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl
strip target/x86_64-unknown-linux-musl/release/nginxy
```

`musl-tools` supplies the linker and `cmake` builds aws-lc-rs (the rustls
backend). The result is a static-pie binary of about 7 MB that starts on
Debian 10+, Ubuntu 20.04+, RHEL 8+ and Alpine - no `GLIBC_2.xx` failures, no
container pinning. `cargo-zigbuild` is the fallback if musl cannot be installed on
a runner.

### 8.2 Windows - MSVC with the static CRT

`cargo build --release --target x86_64-pc-windows-msvc`. The MSVC target links the
C runtime **dynamically** by default, which adds an import of `VCRUNTIME140.dll`
- a DLL that belongs to the Visual C++ Redistributable, not to Windows. On a
machine without it the loader stops before `main()`: no window, no output, not
even `--version`. `.cargo/config.toml` therefore sets
`-C target-feature=+crt-static`, and a workflow step fails the build if
`VCRUNTIME140.dll` or `MSVCP140.dll` reappears in the exe. After that the imports
are only OS DLLs (`kernel32`, `ntdll`, `crypt32`, `bcryptprimitives`, `ws2_32`,
`api-ms-win-core-synch-l1-2-0`), all present on Windows 10 and later.

The exe is unsigned, so a first run may show SmartScreen "More info -> Run anyway";
the README says so.

### 8.3 Artifacts

| Asset | Content |
| --- | --- |
| `nginxy` / `nginxy.exe` | the binary |
| `nginxy.sha256` / `nginxy.exe.sha256` | `<hash>  <filename>\n` - LF endings, no BOM, so `sha256sum -c` works on Linux and under WSL |
| release body | the matching `## <version>` section of `RELEASE-NOTES.md` |

### 8.4 CI shape

1. `actions/checkout`, `dtolnay/rust-toolchain` (stable, plus the target),
   `Swatinem/rust-cache@v2` - v2 is the newest major; a `v3` tag does not exist,
   and asking for it fails the whole job at "Set up job".
2. `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`,
   `cargo test --all-targets`.
3. Release build, `strip` on Linux; sha256 from `sha256sum` or `Get-FileHash`,
   the Windows file written with `[IO.File]::WriteAllText` and a backtick-`n`.
4. Smoke test - the step that proves the *artifact* works, not just the tests:

   ```bash
   ./dist/nginxy --version
   ./dist/nginxy --help > /dev/null
   mkdir -p /tmp/nginxy-conf
   printf 'source:\n  url: "http://127.0.0.1:9/api"\ntargets:\n  - name: "Smoke"\n    url: "http://127.0.0.1:9/api"\n' > /tmp/nginxy-conf/config.yaml
   cp log.yaml /tmp/nginxy-conf/
   printf 'NPM_SOURCE_IDENTITY=smoke@example.com\nNPM_SOURCE_SECRET=smoke\n' > /tmp/nginxy-conf/.env
   ./dist/nginxy --config-dir /tmp/nginxy-conf -p 0    # port 9 refuses: one cycle, then exit
   grep -q "Starting Sync" /tmp/nginxy-conf/logs/*.log # the name comes from log.yaml
   ```

5. `softprops/action-gh-release@v3` on `v*` tags, with the body from `xtask`.
6. Optional hardening: `cargo deny check` and `cargo audit`.

### 8.5 Versioning

`--version` always reports the version of the running binary: `NGINXY_BUILD_VERSION`
if a build step set it, else `CARGO_PKG_VERSION` from `Cargo.toml`, else `unknown`.
That is what lets an operator confirm which executable is installed.

`log.yaml`'s `version:` is a separate marker for the release tooling and for a
deployment that wants to record which configuration it runs; it never changes what
`--version` prints.

`cargo xtask release-notes --tag <tag>` exits 0 on success, 1 when the section is
missing and 2 when the tag and `log.yaml` disagree, so a mistyped tag cannot
publish the wrong notes. A release is a tag push: both build workflows run and
attach their assets to the same release, and a re-run of a failed job is safe.

---

## 9. Risks and mitigations

| Risk | Impact | Mitigation |
| --- | --- | --- |
| YAML edge cases (tabs, anchors, an unquoted `1.1.0`) | A configuration that loads elsewhere is rejected | Fixture tests over realistic files; `version:` is parsed as a `Value` and stringified, so `1.1` and `"1.1"` both print |
| BOM forgotten | Windows-edited configuration rejected | Explicit test with a `\u{feff}` prefix in both files |
| `ProxyHost` drops unknown fields | Silent data loss on target hosts | `#[serde(flatten)] rest: Map<String, Value>` plus a round-trip test asserting every input key survives |
| `1.0` versus `1` in JSON | Spurious `PUT`s or rejected payloads | `serde_json` keeps the integer/float distinction; `arbitrary_precision` stays off |
| A new NPM field is neither propagated nor excluded | Configuration silently not synced | `every_field_is_classified` fails the suite and names the field |
| Rotation semantics drift | Unbounded logs or lost history | Unit tests assert `.1/.2/.3`, deletion beyond `max_backup_count`, and `max_bytes: 0` |
| Ctrl-C during a long period | Shutdown delayed by up to the period | Sliced, interruptible sleep; test asserts exit 130 quickly |
| musl DNS in unusual containers | Hostnames fail where a glibc build worked | Documented; typical configurations use IPs; fallback is a glibc build in a pinned container |
| Self-signed NPM certificates | `--dry-run` fails on TLS | Verification stays on; `insecure_skip_verify` is a deliberate follow-up (Section 11) |
| Windows exe needs a runtime DLL | The program does nothing at all on a clean machine | Static CRT, plus a build step that fails if the import reappears |
| Two workflows attach to one release | A job can fail with "release already exists" | Re-run that job; the release and the other asset are unaffected |

---

## 10. Decisions

| # | Decision | Chosen | Why |
| --- | --- | --- | --- |
| D1 | Name of the binary and assets | `nginxy` | One name everywhere; no compatibility copies |
| D2 | Repository shape | One package: `lib.rs` + `main.rs` + `bin/xtask.rs` | A workspace would add overhead a program this size does not need |
| D3 | Rendering of domain lists in log lines | Rust style, `["a.com", "b.com"]` | `{:?}` on a `Vec<String>` keeps the code plain; the information is unchanged |
| D4 | `--log-level` versus `log_level` | The file applies; the flag overrides when given; no `NONE` | `NONE` only existed because the flag had a default, and a default made the file's value dead |
| D5 | TLS trust for self-signed NPM | Verify only | Secure by default; `insecure_skip_verify` becomes an explicit, documented opt-in |
| D6 | Release-notes tooling | `cargo xtask release-notes` | Keeps CI Rust-only, with exit codes that stop a mis-tagged release |
| D7 | Change detection | Compare the payload, excluding `EXCLUDED_KEYS` and `NOT_SENT` | A deny-list propagates new fields by construction, and `modified_on` no longer triggers writes |
| D8 | Credential variables | `NPM_SOURCE_IDENTITY` / `NPM_SOURCE_SECRET` | One name, matching the per-target `auth_payload` keys; the startup error names anything missing |
| D9 | Config folder when the binary's folder has no `config.yaml` | Use the current directory and log a WARNING | Makes `cargo run` and an editor's Run button work; a deployed binary still checks its own folder first |
| D10 | `${VAR}` placeholders in `config.yaml` and `log.yaml` | Supported, expanded in parsed values only | Configuration can be committed without secrets, and comments stay inert |
| D11 | A target whose write fails | Collect the failures, report them, keep the target queued | A sync is not reported as done when it is not, and the next cycle retries |
| D12 | Windows C runtime | Linked statically | The exe must start on a clean Windows install with nothing added |
| D13 | Log file name | `nginxy.log`, and `log.yaml` remains authoritative | Consistent naming; anyone who prefers another name sets `logging.file_name` |

---

## 11. Open items

**Resolved on 2026-11-19: the live cutover.** A `--dry-run -p 0` beside the
running service reported all 11 hosts in step (zero proposed writes); the
existing systemd unit was restarted onto nginxy 0.1.4 with the same `-p 20`
cadence, and the new process logs `No changes in source and all targets are in
sync.` with no errors. The deployed configuration has a single target
(NAS2-Nginx), so "both production targets" in earlier drafts did not apply.

Open:

1. **`insecure_skip_verify`** (D5) - **parked, not implemented.** A per-endpoint
   boolean, default `false`. When `false` (the default), any `https://` endpoint
   whose certificate is untrusted, or whose name does not match, fails the
   connection; plain `http://` is unaffected. When `true`, certificate *and*
   hostname verification are disabled for that endpoint, so nginxy connects to a
   self-signed / private-CA / IP-addressed HTTPS server and the connection
   succeeds whenever the server answers TLS at all - at the cost of accepting a
   possible man-in-the-middle. Parked because the deployed endpoints are plain
   HTTP, and a properly trusted certificate (system store or `SSL_CERT_FILE`) is
   the preferred fix whenever HTTPS is required.
2. **Optional `cargo xtask schema-check`**: re-run the field classification against
   a live NPM after an upgrade, so a new key is caught in one command instead of
   during the next sync.

---

## 12. Definition of done

1. `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and
   `cargo test --all-targets` clean on Linux and Windows.
2. A configuration folder loads unedited: `config.yaml`, `log.yaml` and `.env`,
   with or without a BOM, with or without placeholders.
3. The log lines of Section 2.3 appear verbatim; the file lands at
   `<config-dir>/logs/nginxy.log` and rotates at the configured size.
4. `--version` works with no configuration; `--config-dir` points elsewhere;
   exit codes 0 / 2 / 130 behave as specified.
5. `--dry-run -p 0` writes nothing and reports exactly what a real run would do.
6. One real cycle leaves every target matching the source, and a second cycle
   logs `No changes in source and all targets are in sync.`
7. The Linux artifact is static musl; the Windows artifact is one `.exe` that
   imports only DLLs Windows itself ships.
8. `sha256` files verify with `sha256sum -c` and `Get-FileHash`; the release body
   equals the matching section of `RELEASE-NOTES.md`.
9. The README documents installing a binary - no build prerequisites for users.






