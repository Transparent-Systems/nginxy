# Release Notes nginxy

## 0.1.3 - A `.env` saved with a BOM is read

- `.env` now goes through the same UTF-8 and BOM handling as `config.yaml` and
  `log.yaml`. A `.env` written by Notepad (UTF-8 with BOM) used to hide its first
  key, so startup stopped with `No credentials configured for: ...` even though
  the file was right there.
- The BOM and invalid-UTF-8 cases are unit-tested for all three files.
- No other behaviour changes.

## 0.1.2 - The log file is nginxy.log, and the documentation is nginxy-only

- The log file is now `logs/nginxy.log`, in the built-in default *and* in the
  shipped `log.yaml`. A folder without a `log.yaml` and one with the sample file
  now agree; before, only the sample used the new name. Anything that tails,
  ships or rotates the file by name needs the new name (`logging.file_name`
  sets it, so a different name is still one line of configuration).
- `doc/PLAN.md` replaces the port plan: it documents the behaviour contract,
  architecture, build/release and the decisions. No part of the documentation
  describes the application this one replaced.
- Comment-only cleanups: source comments, two test names and the `xtask`
  description no longer refer to that application.
- Reading a configuration file strips a UTF-8 BOM at the byte level: same
  behaviour as before, one copy fewer, and both the BOM and the invalid-UTF-8
  cases are now unit-tested.
- No sync behaviour changes: the algorithm, the API calls, the log line format
  and the exit codes are the same as 0.1.1.

## 0.1.1 - The Windows exe starts on a clean install

The 0.1.0 Windows build linked the C runtime dynamically, so `nginxy.exe`
imported `VCRUNTIME140.dll`. That DLL belongs to the Visual C++ Redistributable,
not to Windows, and when it is missing the loader stops *before* `main()`: no
window, no console output, not even `--version`, and no log file. The exe is now
linked with the static CRT and imports only DLLs that every Windows 10/11
installation has.

- `nginxy.exe` no longer depends on the Visual C++ Redistributable - nothing has
  to be installed for it to start.
- The build now fails if that import ever reappears, so a release cannot ship an
  exe that is dead on a clean machine.
- No behaviour changes: the command line, the log format and the sync logic are
  the same as 0.1.0.
- Still on 0.1.0: either install "Microsoft Visual C++ 2015-2022 Redistributable
  (x64)", or copy `VCRUNTIME140.dll` next to the old exe.

## 0.1.0 - First release

Reads `config.yaml`, `log.yaml` and `.env` from one folder and keeps every
target in step with the source.

- Single native binary: no interpreter and no script to install.
- Linux build is statically linked (musl), so it starts on Debian 10+, Ubuntu
  20.04+, RHEL 8+ and Alpine without glibc version problems.
- Windows build is one unsigned `.exe` (Windows 10/11 x64).
- Same command line: `-p/--period`, `-l/--log-level`, `--dry-run`,
  `--config-dir`, `--version`.
- A log file in `<config-dir>/logs/`, with the line format and rotation
  defaults described in `doc/PLAN.md`.
- `--version` still reads the version from `log.yaml`.
- Ctrl-C stops immediately, even in the middle of a 30 second period.
- Domain lists in log lines are rendered as `["a.com", "b.com"]`. The
  information and the message text are unchanged.
- Running from source no longer needs `--config-dir`: if the folder holding the
  binary has no `config.yaml`, the current directory is used and a WARNING names
  it. A deployed binary still reads its own folder first.
- New: `${VAR}` placeholders in `config.yaml` and `log.yaml`, in the style of
  docker-compose (`${VAR}`, `${VAR:-default}`, `${VAR:?message}`, `$$`). Values
  come from the process environment, then `.env`, so the configuration files can
  be committed without addresses or credentials. Only values are expanded -
  comments are ignored - and an unset `${VAR}` stops startup naming the file and
  the setting (`config.yaml: source.url: variable ... is not set`).
- The credential variables are `NPM_SOURCE_IDENTITY` and `NPM_SOURCE_SECRET` -
  the only names accepted, so one spelling is used throughout. A `.env` using
  the shorter `NPM_IDENTITY`/`NPM_SECRET` names must rename its two keys:

  ```bash
  sed -i 's/^NPM_IDENTITY=/NPM_SOURCE_IDENTITY=/; s/^NPM_SECRET=/NPM_SOURCE_SECRET=/' .env
  ```

  Without that, startup stops with `No credentials configured for: source, ...`
  naming the variables it expects.
- Logging no longer aborts the process when stderr is a closed pipe
  (`nginxy ... | head` used to exit 101).
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
  default, so it only overrides when you pass it. The old `NONE` value is gone:
  it existed only because the flag had an `INFO` default, and it is now rejected
  like any other unknown level (exit code 2). Previously a `log_level: DEBUG` was
  ignored unless you knew to pass `-l NONE`.

- A target whose create, update or delete call failed is no longer counted as
  synchronised. The pass reports the failed operations, the target stays in the
  retry list, and the next cycle tries again (log: `1 operation(s) failed on
  target ...; it stays in the retry list`). Previously such a target was only
  retried after the next change on the source, so a failed write could stay
  unnoticed until someone read the log.
