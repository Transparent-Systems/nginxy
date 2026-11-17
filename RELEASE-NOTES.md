# Release Notes nginxy

## 0.1.0 - Rust port of nginx-sync

Rewritten in Rust; reads the same `config.yaml`, `log.yaml` and `.env` from the
same folder as `nginx-sync` and performs the same synchronisation.

- Single native binary, no Python, no interpreter, no bundled C runtime.
- Linux build is statically linked (musl), so it starts on Debian 10+, Ubuntu
  20.04+, RHEL 8+ and Alpine without glibc version problems.
- Windows build is one unsigned `.exe` (Windows 10/11 x64).
- Same command line: `-p/--period`, `-l/--log-level`, `--dry-run`,
  `--config-dir`, `--version`.
- Same log file (`<config-dir>/logs/nginx_sync.log`), same line format, same
  rotation defaults.
- `--version` still reads the version from `log.yaml`.
- Ctrl-C stops immediately, even in the middle of a 30 second period.
- Domain lists in log lines are rendered the Rust way (`["a.com", "b.com"]`)
  instead of Python's `['a.com', 'b.com']`. Information and message text are
  otherwise unchanged.
- Running from source no longer needs `--config-dir`: if the folder holding the
  binary has no `config.yaml`, the current directory is used and a WARNING names
  it. A deployed binary still reads its own folder first.
- New: `${VAR}` placeholders in `config.yaml` and `log.yaml`, in the style of
  docker-compose (`${VAR}`, `${VAR:-default}`, `${VAR:?message}`, `$$`). Values
  come from the process environment, then `.env`, so the configuration files can
  be committed without addresses or credentials. Only values are expanded —
  comments are ignored — and an unset `${VAR}` stops startup naming the file and
  the setting (`config.yaml: source.url: variable … is not set`).
- The credential variables are now `NPM_SOURCE_IDENTITY` and `NPM_SOURCE_SECRET`.
  The previous `NPM_IDENTITY`/`NPM_SECRET` are still read as a fallback, so an
  existing `.env` keeps working.
- Logging no longer aborts the process when stderr is a closed pipe
  (`nginxy … | head` used to exit 101).

Known behaviour inherited from `nginx-sync` v1.1.0 on purpose, to be assessed
after the port has proven itself:

- `--log-level` defaults to `INFO` and overrides `log_level` in `log.yaml`;
  only `-l NONE` lets `log.yaml` win.
- A change anywhere in a source host (including `modified_on`) re-syncs every
  target.
- A target is treated as synchronised once the sync pass finishes, even if one
  of its create/update/delete calls failed; it is retried at the next source
  change.
