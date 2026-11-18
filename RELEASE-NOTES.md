# Release Notes nginxy

## 0.1.1 - The Windows exe starts on a clean install

The 0.1.0 Windows build linked the C runtime dynamically, so `nginxy.exe`
imported `VCRUNTIME140.dll`. That DLL belongs to the Visual C++ Redistributable,
not to Windows, and when it is missing the loader stops *before* `main()`: no
window, no console output, not even `--version`, and no log file. The exe is now
linked with the static CRT and imports only DLLs that every Windows 10/11
installation has.

- `nginxy.exe` no longer depends on the Visual C++ Redistributable — nothing has
  to be installed for it to start.
- The build now fails if that import ever reappears, so a release cannot ship an
  exe that is dead on a clean machine.
- No behaviour changes: the command line, the log format and the sync logic are
  the same as 0.1.0.
- Still on 0.1.0: either install "Microsoft Visual C++ 2015-2022 Redistributable
  (x64)", or copy `VCRUNTIME140.dll` next to the old exe.

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

- A target whose create, update or delete call failed is no longer counted as
  synchronised. The pass reports the failed operations, the target stays in the
  retry list, and the next cycle tries again (log: `1 operation(s) failed on
  target …; it stays in the retry list`). Previously such a target was only
  retried after the next change on the source, so a failed write could stay
  unnoticed until someone read the log.
