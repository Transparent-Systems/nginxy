# nginxy — port plan: `nginx-sync` (Python) → `nginxy` (Rust)

Status: **implemented in v0.1.0** — see §14 for what was built and how each
milestone was verified. Decisions taken during implementation are recorded in
§10 ("chosen" column).
Source application: `/mnt/dev/Development/nginx-sync` (v1.1.0, MIT, author John Zoetebier)
Target folder: `/mnt/dev/Development/nginxy`
Toolchain available: `cargo` / `rustc` 1.99.1; crates.io reachable (`cargo add` verified)

---

## 0. Goal and non-goals

**Goal.** A behaviour-compatible Rust rewrite of `nginx-sync` that can replace the
Python binary on the same machines, reading the *same* `config.yaml`, `log.yaml`
and `.env` from the *same* folder, producing the *same* log file and the *same*
NPM API traffic — but as one small static binary with no Python, no interpreter,
no bundled C runtime.

**Non-goals for v0.1 of the port**

- No new features (no scheduling UI, no metrics endpoint, no config hot-reload).
- No GUI, no database, no daemon-management code.
- No change to the NPM API surface used (`/api/tokens`, `/api/nginx/proxy-hosts`).
- No change to the on-disk config schema — existing user folders must keep working.

Behaviour that looks like a bug but is *contractual* (scripts, CI and operators
depend on it) is listed in §2 and must be preserved. Anything we deliberately
change is recorded in §2.4.

---

## 1. What the Python application actually is

| File | Lines | Role |
| --- | --- | --- |
| `main.py` | 618 | Whole application: CLI, config, logging, NPM client, sync engine |
| `config.yaml` | 12 | `source.url`, `targets[].{name,url,auth_payload?}` |
| `log.yaml` | 8 | `version`, `logging.{file_name,folder,max_bytes,max_backup_count,log_level}` |
| `.env` | 2 | `NPM_IDENTITY`, `NPM_SECRET` (renamed to `NPM_SOURCE_*` in nginxy) |
| `utils/release_notes.py` | ~120 | Extracts one `## <version>` section of `RELEASE-NOTES.md` for a GitHub Release; fails the build when tag ≠ `log.yaml` version |
| `build_project.py` | ~200 | PyInstaller driver (`onefile`/`onedir`), reads version from `log.yaml`, bundles `log.yaml` for `--version` |
| `.github/workflows/build-linux.yml` | — | Debian bullseye container build, smoke test, sha256, release attach |
| `.github/workflows/build-windows.yml` | — | windows-latest build, smoke test, sha256, release attach |
| `README.md`, `RELEASE-NOTES.md`, `LICENSE` | — | Docs (MIT) |

Dependencies: `requests`, `PyYAML`, `python-dotenv`, `PyInstaller`.

### 1.1 Function inventory of `main.py`

| Python symbol | Lines | Responsibility |
| --- | --- | --- |
| `get_script_dir()` | 35-46 | Config folder = folder of the frozen exe, else of `main.py` |
| `load_config()` | 51-65 | Read `config.yaml` as UTF-8-SIG; missing file → 3-line stderr message + exit 2 |
| `setup_logging()` | 67-115 | Rotating file handler (UTF-8) + console handler (`StreamHandler` → **stderr**), format `%(asctime)s - %(levelname)s - %(message)s` |
| `read_version()` | 133-156 | `version:` from `log.yaml`, config dir first, then `sys._MEIPASS`; fallback `"unknown"` |
| `load_settings()` | 159-173 | `.env` → `config.yaml` → `log.yaml`; sets `SETTINGS_DIR`, `CONFIG`, `SOURCE_CONFIG`, `TARGETS`, `logger` |
| `get_auth_payload()` | 181-186 | `auth_payload` from config, else env `NPM_SOURCE_IDENTITY`/`NPM_SOURCE_SECRET` (legacy `NPM_*` still read) |
| `missing_credentials()` | 188-209 | Names endpoints lacking identity/secret; drives exit code 2 |
| `main()` | 211-325 | The sync loop: session map, phases 0-3, `unsynced_targets`, dry-run handling |
| `describe_http_failure()` | 328-336 | `HTTP <code>: <whitespace-collapsed body[:300]>` |
| `get_token()` | 338-374 | `POST {url}/tokens`, 10 s timeout, `{"token": ...}` |
| `add_proxy_host()` | 375-397 | `POST {url}/nginx/proxy-hosts` with filtered payload |
| `update_proxy_host()` | 399-421 | `PUT {url}/nginx/proxy-hosts/{id}` |
| `delete_proxy_host()` | 423-443 | `DELETE {url}/nginx/proxy-hosts/{id}` |
| `sync_to_target()` | 445-518 | GET target hosts, keyed diff, delete pass then create/update pass |
| `should_update()` | 502-517 | Compare 10 named fields only |
| `parse_args()` | 455-494 | `-p/--period`, `-l/--log-level`, `--dry-run`, `--config-dir`, `--version` |
| `_handle_sigint()` / `run()` / `cli()` | 556-614 | Signal guard, settings load, exit codes 0/2/130 |


### 1.2 Data flow (must be reproduced exactly)

```
config.yaml ─┐
.env ────────┼─► Settings ─► per-URL Session (Client + Bearer token)
log.yaml ────┘                     │
                                   ▼
  loop:
    phase 0: every 12 h drop all Authorization headers
    phase 1: GET source /nginx/proxy-hosts  ──► source_hosts (list of objects)
    phase 2: source_hosts != previous_source_hosts ?
               yes → unsynced_targets = all targets; log initial/change
                     (marker only recorded when not --dry-run)
    phase 3: for each unsynced target:
               GET target /nginx/proxy-hosts
               key = sorted(domain_names) tuple  (map of key → host)
               pass A: delete target keys ∉ source keys
               pass B: for each source host: exists && should_update → PUT
                                              exists && equal        → log "up to date"
                                              missing                → POST
    sleep(period) or exit when period <= 0
```

---

## 2. Behavioural contract (the acceptance test for the port)

### 2.1 Command line

| Flag | Type | Default | Notes |
| --- | --- | --- | --- |
| `-p`, `--period <SECONDS>` | integer | `30` | `0` = run one cycle and exit |
| `-l`, `--log-level <LEVEL>` | `DEBUG\|INFO\|WARNING\|ERROR\|CRITICAL\|NONE` | `INFO` | `NONE` means "do not override `log.yaml`" — it does **not** silence logging (see §2.4) |
| `--dry-run` | flag | off | No POST/PUT/DELETE; the source-change marker is not consumed |
| `--config-dir <PATH>` | path | exe folder | Wins over the bundled/next-to-binary files |
| `--version` | flag | — | Must work with **no** configuration present |
| `-h`, `--help` | flag | — | Exit 0 |

Exit codes: `0` success · `2` missing `config.yaml` or missing credentials · `130` Ctrl-C.

### 2.2 Files, names, formats

- Config folder default: directory of the running executable (Rust: `std::env::current_exe()?.parent()`). When that folder has no `config.yaml`, the current working directory is used instead and the choice is logged as a warning (see §2.4 D8).
- `config.yaml`, `log.yaml`, `.env` are read **UTF-8 with optional BOM** (`utf-8-sig`). Rust must strip `\u{FEFF}` before parsing — serde YAML parsers reject a BOM.
- Log file: `<config-dir>/<logging.folder>/<logging.file_name>`, default `logs/nginx_sync.log`; folder created if absent; relative `folder` resolved against the config dir.
- Rotation: rename `x.log` → `x.log.1` … up to `max_backup_count` (default 3), rotate before a write that would exceed `max_bytes` (default 10 485 760); UTF-8, one flush per record.
- Log line format: `YYYY-MM-DD HH:MM:SS,mmm - LEVEL - message` (local time, ms with comma), written to **stderr** on console and to the file.
- Payload filtering: strip `id`, `owner_user_id`, `created_on`, `modified_on`, `user`; **keep every other field verbatim** (including unknown/new NPM fields).
- `should_update()` compares only: `forward_host`, `forward_port`, `forward_scheme`, `advanced_config`, `ssl_forced`, `http2_support`, `block_exploits`, `caching_enabled`, `allow_websocket_upgrade`, `enabled`, with "missing key == null" semantics.
- Host identity for diffing: `domain_names` **sorted**; a target host whose sorted set is absent from source is deleted first (frees names for re-assignment).
- HTTP timeout 10 s per request; NPM errors logged as `HTTP <code>: <body, whitespace-collapsed, max 300 chars>`.

### 2.3 Log strings CI and operators depend on

The Linux/Windows workflows `grep`/`Select-String` for `Starting Sync`, so at
minimum these must survive verbatim:

```
Starting Sync{mode}: {n} targets configured.          (mode = " (DRY RUN MODE)" or "")
Fetching proxy hosts from {endpoint}
Source change detected ({n} hosts).
Initial sync: Source has {n} proxy hosts.
No changes in source and all targets are in sync.
Sync completed. Next sync in {period} seconds.
--- Syncing to Target: {name} ({url}) ---
[DRY RUN] Would create|update|delete host: {domain_names}
Successfully created|updated|deleted proxy host: ...
```

Note `payload.get('domain_names')` renders as a Python list (`['a.com', 'b.com']`).
Rust's `{:?}` on a `Vec<String>` gives `["a.com", "b.com"]`. Per §10-D3 the Rust
form is used, so log lines differ from the Python ones only in those quotes.

### 2.4 Deliberate deviations to document in `RELEASE-NOTES.md`

| # | Python behaviour | Rust behaviour | Why |
| --- | --- | --- | --- |
| D1 | `-l NONE` keeps the `log.yaml` level (does not silence) | same, but `--help` says "keep log.yaml level" | No silent behaviour change; only fix the wording |
| D2 | Ctrl-C raises `KeyboardInterrupt`; later Ctrl-C swallowed | `ctrlc` handler sets an `AtomicBool`; sleeps are interruptible (`recv_timeout`/sliced sleep) so shutdown is immediate; repeats ignored | Same observable behaviour without exception unwinding |
| D3 | list logs as `['a.com', 'b.com']` | **Rust style `["a.com", "b.com"]`** (chosen during implementation) | The characters differ, the information does not; keeping `{:?}` keeps the code plain |
| D4 | PyInstaller onefile unpacks ~1 s per start | native binary starts in ms | Improvement, note only |
| D5 | binary needs the glibc of the build image | musl static build (§8) | Removes the "GLIBC_2.xx not found" class of bug entirely |
| D8 | config folder is always the script's folder, so `python main.py` from the repo works | folder of the executable; **if that has no `config.yaml`, the current directory** (with a WARNING line) | `cargo run` puts the executable in `target/debug`, where the Python rule would never find the repository's files. Deployed binaries are unaffected: their own folder is checked first, and `--config-dir` always wins |
| D9 | no variable interpolation: config values are literal, credentials only via `.env` or `auth_payload` | `${VAR}` / `${VAR:-default}` / `${VAR:?message}` / `$$` in `config.yaml` and `log.yaml`, resolved from the process environment then `.env`, applied to **parsed values** only | Requested so the YAML files can be committed with no secrets, and so CI/deployment variables work without rewriting the config. Additive: literal values and the env credentials behave exactly as before. Expanding the parsed document (not the raw text) keeps documentation comments in `config.yaml` inert — a flaw the first real run exposed |
| D10 | credentials come from `NPM_IDENTITY`/`NPM_SECRET` | `NPM_SOURCE_IDENTITY`/`NPM_SOURCE_SECRET`, with the old names still read as a fallback (new names win) | Requested rename, for symmetry with the per-target `auth_payload` keys. The fallback keeps an existing `nginx-sync` `.env` working, so the rename cannot break a deployment |

### 2.5 Four Python quirks to keep, explicitly

1. `source_hosts != previous_source_hosts` compares **whole objects**, including
   `modified_on`. Touching any host on the source therefore re-syncs *all* targets
   even when nothing else changed. Port this faithfully; a
   `--compare-ignoring <field>` option is a later opt-in, not part of v0.1.
2. `unsynced_targets` only grows on a source change; a target that failed is
   retried every cycle until it succeeds. Keep exactly that.
3. `--log-level` defaults to `INFO`, and `run()` only skips the override when the
   value is `NONE`. So the `log_level` in `log.yaml` is effectively *ignored*
   unless the operator passes `-l NONE`. Any "log.yaml says DEBUG but I see INFO"
   report is this. Port as-is (D4) and document it in `--help` and the README.
4. `sync_to_target()` ignores the return value of `add/update/delete_proxy_host()`,
   so it reports success even when a write failed. The target is then dropped
   from `unsynced_targets` and only retried after the next source change. Ported
   as-is; candidate for a fix once the port is validated.

---

## 3. Dependency choices

| Concern | Crate | Why this one |
| --- | --- | --- |
| CLI | `clap` 4 (`derive`) | De-facto standard; gives `-h`/`--version`/value validation for free. `value_parser` enforces the six log levels |
| HTTP | `reqwest` 0.13, features `blocking`, `json`, `rustls` | The Python app is a sleep-loop, not a server: async buys nothing and costs a runtime. `rustls` avoids OpenSSL (and its MSVC/Windows pain). In 0.13 the feature is named `rustls` (not `rustls-tls`) and uses the aws-lc-rs backend |
| JSON | `serde`, `serde_json` | Needed for API payloads. `serde_json::Value` maps 1:1 onto the Python `dict`; integers stay integers (forward_port 80 does not become 80.0) |
| YAML | `serde_norway` (maintained fork of `serde_yaml` 0.9) | `serde_yaml` is deprecated/unmaintained; `serde_norway` is API-identical so a switch back or to `serde_yml` is a one-line change |
| `.env` | `dotenvy` | `python-dotenv` semantics: existing process env wins, no override |
| Logging | `log` (feature `std`) + hand-written `Log` impl (~80 LOC) | `env_logger`/`tracing-subscriber` cannot reproduce `YYYY-MM-DD HH:MM:SS,mmm - LEVEL - msg` + `RotatingFileHandler` semantics without more code than writing it. `std` must be enabled explicitly for `set_boxed_logger` |
| Timestamps | `chrono` (`clock`) | Local time + millisecond formatting; `time` crate is the alternative if `chrono` is unwanted |
| Ctrl-C | `ctrlc` | Portable SIGINT/SIGINT-equivalent handler on Windows too |
| Errors | `thiserror` (library), plain `enum`/`ExitCode` in `main` | Keeps error types explicit; `anyhow` only if it stays out of library APIs |
| Tests | `mockito` (blocking HTTP mock), `tempfile` | Matches the blocking client; `wiremock` would drag in a tokio runtime for no gain |

Avoid: `tokio`/`axum` (no server), `async-std`, `openssl`, `serde_yaml` (EOL),
`lazy_static` (use `OnceLock`), `error-chain`.

Minimum supported Rust: **1.85** (`rust-version` in `Cargo.toml`). 1.75 was the
first choice, but it forces `reqwest` 0.13.3 and pins the rest of the tree to
older releases; 1.85 is still comfortably below the 1.99.1 installed here.

---

## 4. Target project layout

One package with a library plus a thin binary — mirrors the layering of the
existing `monly` workspace (`core` logic separate from the CLI) without the
multi-crate overhead a ≈900-line program does not need. If a second front end
(GUI, service wrapper) ever appears, `nginxy-core` becomes a `crates/` member
without touching the code below.

```
/mnt/dev/Development/nginxy/
├── Cargo.toml                 # package nginxy, lib + bin
├── Cargo.lock                 # committed (binary project)
├── rust-toolchain.toml        # channel = "stable", components fmt+clippy
├── .gitignore                 # /target, .env, logs/, dist/
├── README.md                  # rewritten Quick Start (no Python prerequisite)
├── RELEASE-NOTES.md
├── LICENSE                    # MIT, same author line
├── PLAN.md                    # this document
├── config.yaml                # sample, identical schema to nginx-sync
├── log.yaml                   # sample, identical schema (incl. version:)
├── src/
│   ├── main.rs                # arg parse → exit code; Ctrl-C; ~60 lines
│   ├── lib.rs                 # re-exports behind `pub mod`
│   ├── cli.rs                 # clap derive `Args`, `LogLevel`
│   ├── settings.rs            # ConfigDir resolution, YAML models, .env, BOM strip
│   ├── logging.rs             # init_logger(), RotatingWriter, format_record()
│   ├── http.rs                # Session (Client + token), describe_http_failure()
│   ├── npm.rs                 # token(), list/create/update/delete proxy host
│   ├── host.rs                # ProxyHost model, DomainKey, payload(), should_update()
│   ├── sync.rs                # Engine::run() — phases 0-3, unsynced set, dry-run
│   └── version.rs             # read_version(): config dir → embedded → "unknown"
├── tests/
│   ├── config_parity.rs       # same config.yaml/log.yaml as Python, incl. BOM
│   ├── npm_api.rs             # mockito: create/update/delete/dry-run sequences
│   └── cli.rs                 # --version/--help with no config; exit codes
├── xtask/
│   ├── Cargo.toml             # `cargo xtask release-notes --tag v1.1.0`
│   └── src/main.rs            # port of utils/release_notes.py
└── .github/workflows/
    ├── ci.yml                 # fmt, clippy -D warnings, test (linux+windows)
    ├── build-linux.yml        # x86_64-unknown-linux-musl, smoke test, sha256, release
    └── build-windows.yml      # x86_64-pc-windows-msvc, smoke test, sha256, release
```

## 5. Python → Rust mapping

| Python | Rust | Notes |
| --- | --- | --- |
| `get_script_dir()` | `settings::exe_dir()` | `current_exe()?.parent()`; no `sys.frozen` branch needed |
| `load_config()` | `settings::Config::load(&Path)` | `read_to_string` → `strip_prefix('\u{feff}')` → `serde_norway::from_str` |
| `SETTINGS_DIR`/`CONFIG`/`TARGETS` globals | `Settings` struct passed by reference (`&Settings`) | Kills the "import-time nothing is readable" comment and the mutable-global hazard |
| `setup_logging()` | `logging::init(&LogConfig) -> LoggerGuard` | Custom `log::Log` impl; guard keeps the `File` alive; `set_max_level` from `-l`/`log.yaml` |
| `RotatingFileHandler` | `logging::RotatingWriter` | rotate-then-write, `.1 .. .N` renames, delete beyond `max_backup_count` |
| `read_version()` | `version::read(&Path)` | config dir → `option_env!("NGINXY_EMBEDDED_VERSION")` → `"unknown"`; `build.rs` or CI can set it |
| `requests.Session` | `http::Session { client: reqwest::blocking::Client, token: Option<String> }` | `Auth` header applied per request, not stored globally, so "invalidate session" is `self.token = None` |
| `get_auth_payload()` | `settings::Endpoint::auth_payload(&env)` | handles `auth_payload` as mapping *and* JSON-string (both exist in the wild) |
| `missing_credentials()` | `settings::Settings::missing_credentials()` | returns `Vec<String>` for the exit-2 message |
| `main()` loop | `sync::Engine::run(&mut self)` | Phases as one `match`; `unsynced: HashSet<String>`; `previous: Option<Vec<ProxyHost>>` |
| `describe_http_failure()` | `http::describe_failure(&Response)` | body text, `split_whitespace().join(" ")`, truncate at 300 chars on a char boundary |
| `add/update/delete_proxy_host()` | `npm::Npm<'_>` methods | each returns `Result<Value, NpmError>`; caller decides to invalidate the token |
| `should_update()` | `host::should_update(&ProxyHost, &ProxyHost)` | table of 10 keys; `Option<&Value>` comparison gives Python's `None == missing` |
| `sync_to_target()` | `sync::sync_to_target(...)` | `BTreeMap<DomainKey, ProxyHost>` so pass order is deterministic (Python dict order = insertion order) |
| `parse_args()` | `cli::Args` (clap derive) | same short/long flags, same defaults, same `NONE` handling |
| `_handle_sigint()` / `KeyboardInterrupt` | `ctrlc::set_handler` + `stop: Arc<AtomicBool>` | `sleep_unless_stopped(d)` waits on a `mpsc` channel with `recv_timeout` |
| `utils/release_notes.py` | `xtask` subcommand | Removes Python from the release job; same exit codes 0/1/2 |
| `build_project.py` | (not needed) | `cargo build --release --target …` is the whole build |

### 5.1 Data model decision: `ProxyHost`

Do **not** use a closed struct with 20 fields. NPM adds fields between releases and
the app must forward them untouched.

```rust
pub struct ProxyHost {
    pub id: Option<i64>,                 // absent on the source → create path
    pub domain_names: Vec<String>,       // required by both diff and payload
    #[serde(flatten)]
    pub rest: serde_json::Map<String, Value>,  // everything else, verbatim
}
```

`payload()` = `rest` minus `EXCLUDED_KEYS`, plus `domain_names`; the 10
`should_update` fields are read out of `rest` with `.get(key)` so "missing" and
"null" compare equal, exactly like `dict.get()`.

---

## 6. Milestones

Each milestone is independently mergeable and leaves `main` building and tested.
"Parity check" = run Rust and Python against the same mock NPM and diff the log
files with timestamps stripped.

### M0 — Scaffold (½ day)
`cargo new --lib` + `[[bin]]`, `rust-toolchain.toml`, `.gitignore`, `clippy.toml`,
CI job that only runs `fmt`/`clippy`/`test`.
**Done when:** `cargo test` and `cargo clippy -- -D warnings` pass on an empty
skeleton, and `Cargo.lock` is committed.

### M1 — Settings + version (1 day)
`settings.rs`, `version.rs`, sample `config.yaml`/`log.yaml`, BOM handling,
`--config-dir`, missing-config message, `missing_credentials()`.
**Done when:** `tests/config_parity.rs` loads the *literal* files from
`/mnt/dev/Development/nginx-sync/{config.yaml,log.yaml}` (copied into the test as
fixtures) plus a BOM-prefixed copy, and `--version` prints with zero config files
present. Exit code 2 for neither config nor credentials.

### M2 — Logging (1 day)
`logging.rs`: exact record format, stderr + rotating file, folder creation,
level from `log.yaml`, `-l` override, `NONE` semantics.
**Done when:** a test fills a 200-byte budget file and asserts the `.1/.2/.3`
rotation names and that `.4` is removed; a test asserts the line shape with a
regex `^\d{4}-\d\d-\d\d \d\d:\d\d:\d\d,\d{3} - [A-Z]+ - .+$`.

### M3 — NPM client (1½ days)
`http.rs` + `npm.rs`: token fetch, 10 s timeout, `/tokens` error text, list,
create, update, delete, token invalidation on transport error.
**Done when:** `tests/npm_api.rs` (mockito) asserts method, path, `Authorization`
header, JSON body with excluded keys removed, and that a 400 body is surfaced as
`HTTP 400: <detail>`.

### M4 — Sync engine, one-shot (2 days)
`host.rs` + `sync.rs`: domain-set keying, delete-then-create/update ordering,
`should_update` field list, `--dry-run`, `-p 0` exit path, `unsynced_targets`.
**Done when:** scenarios in `tests/npm_api.rs` pass:
1. empty target → N creates, 0 deletes;
2. identical sets → 0 writes (only "up to date" log lines);
3. changed `forward_port` → exactly 1 PUT, no POST/DELETE;
4. target-only domain set → DELETE happens **before** the POST that reuses the name;
5. `--dry-run` → 0 mutating requests, `[DRY RUN]` lines present.

### M5 — Loop behaviour + signals (1 day)
Phase 0 (12 h), phase 2 change detection, "no changes" fast path, retry of failed
targets, incremental log line for unsynced retry, Ctrl-C during a long sleep.
**Done when:** an integration test with a shortened refresh interval shows the
source is fetched each cycle, targets only when needed; a test sends SIGINT to the
child process and asserts exit 130 plus the graceful message within ~200 ms.

### M6 — Packaging + docs (1 day)
README rewrite (install a binary, not Python), `dist/` naming, sha256 files,
`xtask release-notes`, sample config in repo root.
**Done when:** a fresh checkout builds `dist/nginxy` and `dist/nginxy.sha256`, and
the README's copy-paste blocks are the ones actually tested in CI.

### M7 — Cutover validation (½–1 day, on real servers)
Run `nginxy --dry-run -p 0 --config-dir /etc/nginx-sync` next to the Python tool
and diff the log files (timestamps stripped); then run for one cycle for real
against the NAS2/DEV targets, then swap the service unit.
**Done when:** log diffs contain no unexplained lines and a second consecutive
real cycle reports "No changes in source and all targets are in sync."

Total: **≈8–9 working days**, of which M0-M5 is the actual port.

---

## 7. Testing strategy

| Level | Tool | What it locks down |
| --- | --- | --- |
| Unit | `#[test]` in module | `should_update` matrix (each of 10 keys, missing-vs-null), `DomainKey` sorting/duplicates, payload exclusion, `describe_failure` truncation at a multibyte char |
| Fixture parity | `include_str!` of copies of the Python repo's `config.yaml`/`log.yaml` | Config schema never drifts |
| HTTP contract | `mockito` | Paths, verbs, headers, bodies, delete-before-create ordering, dry-run silence |
| Process | `assert_cmd` + `predicates` + `tempfile` | `--version`/`--help` without config, exit 2 cases, log file actually written into `--config-dir` |
| Signals | `std::process::Child` + `libc::kill(pid, SIGINT)` on Unix (`#[cfg(unix)]` test) | exit 130, graceful message, shutdown < 200 ms |
| Cross-OS | CI matrix `ubuntu-latest`, `windows-latest` | Windows path handling, stderr, CRLF-free sha256 file |

Deliberately **not** tested: real NPM servers (they are environment-specific) and
timing-sensitive 12 h refresh (the interval becomes a `Duration` field on the
engine so tests can set 50 ms).


---

## 8. Build and release

### 8.1 Linux — musl instead of a bullseye container

The current workflow builds inside `python:3.12-bullseye` purely to pin glibc
2.31. A Rust binary has no interpreter, but it is still dynamically linked to
glibc by default, so the same trap exists. Fix it properly:

```bash
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl
# → target/x86_64-unknown-linux-musl/release/nginxy  (static, ~5 MB)
strip target/x86_64-unknown-linux-musl/release/nginxy
```

With `rustls` (no OpenSSL) the result is fully static, runs on Debian 10+, Ubuntu
20.04+, RHEL 8+, Alpine — no container pinning, no `GLIBC_2.xx not found`.
`cargo-zigbuild` is the fallback if a musl toolchain cannot be installed on the
runner.

### 8.2 Windows

`cargo build --release --target x86_64-pc-windows-msvc` on `windows-latest`; the
MSVC target needs no extra runtime (the C runtime is part of Windows 10+). Single
`nginxy.exe`, still unsigned → SmartScreen "More info → Run anyway" note stays in
the README.

### 8.3 Artifacts (keep the Python release shape so existing scripts work)

| Asset | Content |
| --- | --- |
| `nginxy` / `nginxy.exe` | the binary |
| `nginxy.sha256` / `nginxy.exe.sha256` | `<hash>  <filename>\n`, LF endings, no BOM (so `sha256sum -c` works on Linux and WSL) |
| release body | matching section of `RELEASE-NOTES.md`, produced by `cargo xtask release-notes --tag <tag>` |

Decision §10-D1 covers whether the asset keeps the name `nginx-sync` for a
drop-in replacement.

### 8.4 CI workflow shape (replaces both PyInstaller workflows)

1. checkout, `dtolnay/rust-toolchain` (stable + target), `Swatinem/rust-cache`
2. `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`
3. release build, `strip` (Linux), sha256 (PowerShell `Get-FileHash` on Windows,
   written with `[IO.File]::WriteAllText` + `` `n `` — same trick the Python
   workflow already uses)
4. smoke test — same three checks the Python workflow does, which is the whole
   point of keeping the log strings:
   ```bash
   ./dist/nginxy --version
   ./dist/nginxy --help > /dev/null
   mkdir -p /tmp/nginxy-conf
   printf 'source:\n  url: "http://127.0.0.1:9/api"\ntargets:\n  - name: "Smoke"\n    url: "http://127.0.0.1:9/api"\n' > /tmp/nginxy-conf/config.yaml
   cp log.yaml /tmp/nginxy-conf/
   printf 'NPM_SOURCE_IDENTITY=smoke@example.com\nNPM_SOURCE_SECRET=smoke\n' > /tmp/nginxy-conf/.env
   ./dist/nginxy --config-dir /tmp/nginxy-conf -p 0   # port 9 refuses → exits at once
   grep -q "Starting Sync" /tmp/nginxy-conf/logs/nginx_sync.log
   ```
5. `softprops/action-gh-release@v3` on `v*` tags with the notes from `xtask`
6. optional hardening: `cargo deny check` (licences/advisories) and `cargo audit`

### 8.5 Versioning

Single source of truth stays `log.yaml`'s `version:` for *user-visible* version,
so `--version` and the release notes cannot disagree — but that file is user
config, so:
- `--version` prints `<binary-name> <version>` where version =
  `--config-dir/log.yaml` → else the version embedded at build time
  (`NGINXY_BUILD_VERSION` via `build.rs`) → else `CARGO_PKG_VERSION` → else `unknown`.
- `xtask release-notes` keeps the Python script's exit codes (0 ok, 1 no section,
  2 tag/version mismatch) so the release job cannot publish wrong notes.

---

## 9. Risks and mitigations

| Risk | Impact | Mitigation |
| --- | --- | --- |
| `serde_norway`/`serde_yaml` differ on odd YAML (tabs, anchors, `1.1.0` unquoted as string) | Configs that load in Python fail in Rust | Fixture test with the real files; `version: 1.1.0` must stay a string — parse the version as `Value` and stringify (as Python's `str(version)` does) |
| BOM handling forgotten | Windows-edited configs rejected | Explicit fixture test with `\u{feff}` prefix |
| `ProxyHost` struct drops unknown fields | Silent data loss on target hosts | `#[serde(flatten)] rest: Map<String, Value>` + round-trip test asserting all input keys survive |
| JSON numbers: `1.0` vs `1` | Spurious PUTs or rejected payloads | `serde_json` keeps int/float distinction and `arbitrary_precision` is *not* enabled, matching Python's `int`/`float` |
| `should_update` key list drifts from Python | Different write behaviour after upgrade | One `const SHOULD_UPDATE_KEYS: [&str; 10]` plus a unit test listing the Python field names verbatim |
| Log rotation semantics differ (size accounting, `.1` vs `.log.1`) | Grew-unbounded logs or lost history | Port Python's rule exactly (rotate when `current + len > max_bytes`), test it |
| Ctrl-C during `sleep` | Slow shutdown (up to 30 s) | `recv_timeout` loop with 100 ms slices, test asserts < 200 ms |
| musl build lacks DNS/`/etc/resolv.conf` handling in odd containers | Hostnames fail to resolve where Python worked | Documented in README; `--config-dir` configs already use IPs; fallback to a glibc build inside `debian:bullseye` if reported |
| reqwest default headers/differing TLS stack | TLS-only NPM instances with self-signed certs behave differently | v0.1 keeps verification on (as `requests` does); an explicit `insecure_skip_verify` config key is a **separate, documented** feature request, not a silent default |
| Windows service integration unchanged | None expected | README keeps the Task Scheduler/NSSM guidance, just with the new exe name |


---

## 10. Decisions (resolved during implementation)

| # | Question | Options | Chosen |
| --- | --- | --- | --- |
| D1 | Binary/asset name | (a) `nginxy`; (b) keep `nginx-sync`; (c) ship both | **(a) `nginxy`** — one name, no compatibility copies |
| D2 | Repo shape | (a) single package lib+bin; (b) workspace `nginxy-core` + `nginxy-cli` | **(a)** single package, `src/lib.rs` + `src/main.rs` + `src/bin/xtask.rs` |
| D3 | Log rendering of domain lists | (a) Python-style `['a.com']`; (b) Rust `["a.com"]` | **(b)** — `{:?}` on the `Vec<String>`, no quoting helper |
| D4 | `-l NONE` semantics | (a) preserve "don't override"; (b) make it truly silent | **(a)** + clarified `--help`, README and `log.yaml` comment |
| D5 | TLS trust for self-signed NPM | (a) verify only (as `requests`); (b) add `insecure_skip_verify` | **(a)** for v0.1.0; (b) is a follow-up flag |
| D6 | Keep the Python app shipping? | (a) freeze `nginx-sync` at v1.1.0; (b) maintain both | **(a)** |
| D7 | Release-notes tooling | (a) `cargo xtask` in Rust; (b) call the Python script from CI | **(a)** `cargo xtask release-notes` |

---

## 11. Effort summary

| Milestone | Estimate |
| --- | --- |
| M0 scaffold + CI skeleton | 0.5 d |
| M1 settings/version | 1 d |
| M2 logging | 1 d |
| M3 NPM client | 1.5 d |
| M4 sync engine (one-shot) | 2 d |
| M5 loop + signals | 1 d |
| M6 packaging/docs | 1 d |
| M7 real-server cutover validation | 0.5–1 d |
| **Total** | **≈8–9 days** |

Deliverable size: **≈900–1100 lines of Rust** (including tests), replacing 618
lines of Python **plus** `build_project.py` and `utils/release_notes.py`.

---

## 12. Definition of done

1. `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`
   all clean on Linux and Windows.
2. Byte-for-byte the same **config surface**: an unmodified `config.yaml`,
   `log.yaml`, `.env` folder from `nginx-sync` v1.1.0 works with no edits.
3. Log lines in §2.3 appear verbatim; the log file lands at
   `<config-dir>/logs/nginx_sync.log` and rotates at the configured size.
4. `nginxy --version` works with no config files; `--config-dir` points elsewhere;
   exit codes 0 / 2 / 130 match.
5. `--dry-run -p 0` against a live source produces the same set of
   `[DRY RUN] Would …` lines as `main.py --dry-run -p 0`.
6. One real cycle against the two configured targets (NAS2-Nginx, DEV-Nginx)
   creates/updates/deletes nothing unexpectedly, and a second cycle logs
   `No changes in source and all targets are in sync.`
7. Linux artifact is a static musl binary that starts on Debian 11 and Ubuntu
   24.04 without any runtime; Windows artifact is one unsigned `.exe`.
8. `SHA256` files verify with `sha256sum -c` / `Get-FileHash`; the GitHub Release
   body equals the `## v<x.y.z>` section of `RELEASE-NOTES.md`.
9. README documents the binary install only — no Python, no virtualenv, no
   PyInstaller instructions remain.

---

## 13. First implementation step (executed)

```bash
cd /mnt/dev/Development/nginxy
cargo init --name nginxy --lib .
cargo add clap --features derive
cargo add reqwest --no-default-features --features blocking,json,rustls
cargo add serde --features derive
cargo add serde_json serde_norway dotenvy thiserror
cargo add log --features std
cargo add chrono --no-default-features --features clock,std
cargo add ctrlc
cargo add --dev mockito tempfile assert_cmd predicates
```

(`rustls-tls` no longer exists in reqwest 0.13; `log` needs its `std` feature for
`set_boxed_logger`. Both were found by the compiler, not assumed.)

---

## 14. What was built

```
src/main.rs        entry point (ExitCode)
src/cli.rs         Args (clap), exit codes 0/2/130, SIGINT install
src/settings.rs    config.yaml, log.yaml, .env, BOM strip, auth resolution
src/logging.rs     log::Log impl, Python line format, RotatingFile
src/http.rs        Session (blocking reqwest + bearer token), describe_failure
src/npm.rs         token, list, create, update, delete
src/host.rs        ProxyHost, payload filtering, DomainKey, should_update
src/sync.rs        Engine: phases 0-3, unsynced set, dry run
src/stop.rs        interruptible sleep (Ctrl-C)
src/version.rs     --version from log.yaml → build version → crate version
src/bin/xtask.rs   cargo xtask release-notes (port of utils/release_notes.py)
tests/             config_parity (14), npm_api (7), cli (9)
.github/workflows/ ci.yml, build-linux.yml (musl), build-windows.yml
```

### How each milestone was verified

| Milestone | Evidence |
| --- | --- |
| M0 scaffold | `cargo fmt --check`, `cargo clippy --all-targets -D warnings`, `cargo test` all clean |
| M1 settings/version | `tests/config_parity.rs` (14 tests): the shipped Python `config.yaml`/`log.yaml` load unchanged, BOM accepted, `--version` works with no config |
| M2 logging | unit tests for the exact line format and for `.1/.2/.3` rotation with `max_bytes`/`max_backup_count` |
| M3 NPM client | `tests/npm_api.rs`: bearer header, paths, verbs, payload subset, `HTTP 400` body surfaced in the log |
| M4 sync engine | `tests/npm_api.rs`: create, update, unchanged (no writes), delete, dry run (no writes) |
| M5 loop + signals | `tests/cli.rs::ctrl_c_exits_with_130_during_a_long_period` — exit 130 during a 300 s sleep (measured 4 ms by hand) |
| M6 packaging | `cargo build --release` → 8.6 MB binary; `--version`, `--help`, `-p 0` smoke run writes `logs/nginx_sync.log` with `Starting Sync` |
| M6 release notes | `cargo xtask release-notes` prints the 0.1.0 section; `--tag v9.9.9` exits 2, `--version 7.7.7` exits 1 |
| M7 cutover | still to do on the real servers (`--dry-run -p 0` next to `nginx-sync`, then diff the logs) |

`cargo test --all-targets` reports **59 passing tests**: 26 library unit tests,
3 `xtask` tests, 14 configuration-parity tests, 7 mock-NPM API tests and 9
process-level CLI tests.

### Still open (deliberately, to be assessed after real-server validation)

- The inherited quirks in §2.5, including the `-l`/`log.yaml` precedence, the
  whole-object change detection, and a target being counted as synchronised even
  when one of its writes failed.
- `insecure_skip_verify` for self-signed NPM instances (D5).
- The `nginx-sync` → `nginxy` cutover on the two live targets (M7).



