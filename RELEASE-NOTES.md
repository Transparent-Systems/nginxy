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
- The credential variables are `NPM_SOURCE_IDENTITY` and `NPM_SOURCE_SECRET` —
  the only names accepted, so one spelling is used throughout. A `.env` copied
  from `nginx-sync` must have its two keys renamed:

  ```bash
  sed -i 's/^NPM_IDENTITY=/NPM_SOURCE_IDENTITY=/; s/^NPM_SECRET=/NPM_SOURCE_SECRET=/' .env
  ```

  Without that, startup stops with `No credentials configured for: source, …`
  naming the variables it expects.
- Logging no longer aborts the process when stderr is a closed pipe
  (`nginxy … | head` used to exit 101).
- A host update is now decided by comparing the **payload** that would be sent,
  not ten hand-picked fields. `locations` (custom paths), `hsts_enabled`,
  `hsts_subdomains` and `trust_forwarded_proto` therefore propagate, and a field
  added by a future NPM version propagates without a code change.
- `certificate_id`, `access_list_id` and `meta` are no longer sent to targets:
  the first two are identifiers inside one NPM database, the third is runtime
  status. A differing certificate or access list is named in the log instead of
  being ignored silently; previously the source's `certificate_id` was copied to
  every target.
- Touching a host on the source (renaming it, saving it unchanged) no longer
  re-syncs every target: `modified_on` is not part of the comparison.
- `log_level` in `log.yaml` now takes effect on its own. `--log-level` has no
  default, so it only overrides when you pass it. The Python app's `NONE` value
  is gone: it existed only because the flag had an `INFO` default, and it is now
  rejected like any other unknown level (exit code 2). Previously a
  `log_level: DEBUG` was ignored unless you knew to pass `-l NONE`.

Known behaviour inherited from `nginx-sync` v1.1.0 on purpose, still to be
assessed:

- A target is treated as synchronised once the sync pass finishes, even if one
  of its create/update/delete calls failed; it is retried at the next source
  change.
