# nginxy

A Rust rewrite of [nginx-sync](https://github.com/Transparent-Systems/nginx-sync):
it keeps the proxy host configuration of one source Nginx Proxy Manager (NPM)
instance in step with one or more target instances.

`nginxy` reads a `config.yaml`, `log.yaml` and `.env`, from the same
folder, and writes a log file. The main difference is the binary: one native
executable, no Python, no interpreter, no bundled C runtime. The Linux build is
statically linked, so no `GLIBC_2.xx` surprises.

## Quick start

1. **Download** `nginxy` (Linux) or `nginxy.exe` (Windows) from the
   [Releases](https://github.com/Transparent-Systems/nginxy/releases) page, next
   to `nginxy.sha256` / `nginxy.exe.sha256`.

2. **Verify and make it executable** (Linux — the executable bit is not kept by
   the download):

    ```bash
    sha256sum -c nginxy.sha256
    chmod +x nginxy
    ```

    Windows:

    ```powershell
    Get-FileHash .\nginxy.exe -Algorithm SHA256
    ```

3. **Put the three configuration files next to it** — they are read from the
   binary's own folder, not from the current directory:

    ```bash
    cp config.yaml log.yaml nginxy /opt/nginxy/
    printf 'NPM_SOURCE_IDENTITY=your_identity\nNPM_SOURCE_SECRET=your_secret\n' > /opt/nginxy/.env
    /opt/nginxy/nginxy -p 30
    ```

    To keep the configuration somewhere else (binary in `/usr/local/bin`, for
    example), name that folder instead:

    ```bash
    nginxy --config-dir /etc/nginxy -p 30
    ```

## Script arguments

Command line arguments override properties in `config.yaml`.

| Argument | Description |
| -------- | ----------- |
| `-p`, `--period <SECONDS>` | Seconds between syncs (default 30; `0` syncs once and exits) |
| `-l`, `--log-level <LEVEL>` | `DEBUG`, `INFO`, `WARNING`, `ERROR` or `CRITICAL`; overrides `log_level` from `log.yaml` |
| `--dry-run` | Report what would change; the target servers are left untouched |
| `--config-dir <PATH>` | Folder holding `config.yaml`, `log.yaml` and `.env` (default: the folder of the binary) |
| `--version` | Print the version recorded in `log.yaml` and exit |

Without `-l`, the `log_level` from `log.yaml` applies. There is no `NONE`
value: the Python app needed one because its flag carried an `INFO` default,
which `nginxy` does not.

## Configuration

`config.yaml` — source and targets:

```yaml
source:
  url: "http://192.0.2.10:81/api"

targets:
  - name: "NAS2-Nginx"
    url: "http://192.0.2.11:81/api"
  - name: "DEV-Nginx"
    url: "http://192.0.2.12:81/api"
    auth_payload:            # optional; otherwise .env is used
      identity: "admin"
      secret: "change-me"
```

`.env` — credentials used by every endpoint without its own `auth_payload`:

```
NPM_SOURCE_IDENTITY=your_identity
NPM_SOURCE_SECRET=your_secret
```

`log.yaml` — log file location, rotation and the version reported by
`--version`:

```yaml
version: 0.1.2
logging:
  file_name: nginxy.log
  folder: logs
  max_bytes: 10485760
  max_backup_count: 3
  log_level: INFO
```

Logs go to `<config-dir>/logs/nginxy.log` and to stderr, in the line format
`2026-11-18 08:12:33,123 - INFO - message`.

### Variables in the configuration files

`config.yaml` and `log.yaml` may contain `${VAR}` placeholders, so the committed
files carry no addresses or secrets at all — the same pattern `docker-compose`
uses with its `.env`:

```yaml
source:
  url: "${NPM_SOURCE_URL}"

targets:
  - name: "NAS2-Nginx"
    url: "${NPM_TARGET_URL}"
    auth_payload:
      identity: "${NPM_TARGET_IDENTITY}"
      secret: "${NPM_TARGET_SECRET}"
```

```
NPM_SOURCE_URL=http://192.0.2.10:81/api
NPM_TARGET_URL=http://192.0.2.11:81/api
NPM_TARGET_IDENTITY=admin
NPM_TARGET_SECRET=change-me
```

| Syntax | Meaning |
| --- | --- |
| `${NAME}` | value of `NAME`; **startup error** when it is unset |
| `${NAME:-fallback}` | value of `NAME`, or `fallback` when unset or empty |
| `${NAME:?message}` | value of `NAME`, or a startup error carrying `message` |
| `$$` | a literal `$` |

Rules worth knowing:

- Values are read from the **process environment first**, then from `.env` in
  the configuration folder — the same order and the same map the credentials use.
  A service manager that exports variables therefore needs no `.env` at all.
- Expansion is a single pass over the file text: a value that itself contains
  `$` is inserted as-is and never re-expanded.
- An unresolvable placeholder in `config.yaml` stops startup with the file, line
  and the value that needed it (`config.yaml: source.url: variable NPM_SOURCE_URL is not set …`).
  In `log.yaml` it is treated like any other logging typo: the defaults apply.
- Only **values** are expanded, never comments: a documentation block may show
  `${VAR}` without asking for a value.
- `NPM_SOURCE_IDENTITY`/`NPM_SOURCE_SECRET` still work without any placeholder, so
  a folder needs no placeholders at all.
  
## What a sync does

For every target, on every cycle in which the source changed (or the target has
not been synchronised yet):

1. Read the target's proxy hosts.
2. Delete the hosts whose set of domain names no longer exists on the source, so
   their names are free for step 3.
3. Create the hosts that are missing, and update the ones whose **payload**
   differs.

A payload is every field NPM returns, minus the ones that cannot be copied to
another instance:

| Never sent | Why |
| --- | --- |
| `id`, `owner_user_id`, `created_on`, `modified_on`, `user` | read-only |
| `certificate_id`, `access_list_id` | identifiers inside *one* NPM database — the same number means a different certificate or access list on another instance |
| `meta` | runtime status (`nginx_online`, `nginx_err`), not configuration |

Everything else is configuration and is propagated, including `locations`
(custom paths) and `trust_forwarded_proto`. That is a **deny-list, not a list of
known fields**: a field added by a future NPM version is compared and forwarded
automatically, and the test suite fails if a field is ever neither propagated
nor deliberately excluded.

Two consequences worth knowing:

- `modified_on` is not part of the payload, so touching a host on the source —
  renaming it, saving it unchanged — no longer re-syncs every target.
- When a `certificate_id` or `access_list_id` differs between source and target,
  the log says so instead of silently ignoring it; those values are per-instance
  and are never copied.

Hosts are matched by their *sorted* `domain_names`, so the order of domains does
not matter.

If a create, update or delete fails on a target, the pass reports the failed
operations and the target stays queued: the next cycle retries it, without
waiting for the source to change.

## Running from source

`nginxy` looks for `config.yaml` in the folder of the **executable**, so a plain
`cargo run` would search `target/debug/`. When that folder has no `config.yaml`,
it falls back to the current directory and says so in the log — which is what
makes the editor's Run button, F5 and `cargo run` work from the repository root:

```bash
cargo run -- -p 0                        # one sync, then exit
cargo run -- -p 30                       # the normal loop; Ctrl-C stops it
cargo run -- --dry-run -p 0              # report only, write nothing
cargo run -- --config-dir /etc/nginxy -p 30
```

You still need credentials. Create `.env` in the folder being read:

```bash
printf 'NPM_SOURCE_IDENTITY=your_identity\nNPM_SOURCE_SECRET=your_secret\n' > .env
```

`.env` is git-ignored.

```bash
cargo run -- --config-dir ../nginxy --dry-run -p 0
```

### In VS Code

`.vscode/` carries three shared files, so everyone gets the same setup. Open the
**`nginxy` folder itself** as the workspace (`File → Open Folder…`); VS Code only
reads `.vscode/*.json` from the folder currently open.

| File | What it gives you |
| --- | --- |
| `tasks.json` | Tasks: **nginxy: dry run, one sync**, **nginxy: sync every 30 s**, **nginxy: version**, build/test/clippy/fmt |
| `launch.json` | F5 → *nginxy: one dry-run sync (F5)* with breakpoints (CodeLLDB on Linux/macOS, `cppvsdbg` on Windows, plain `gdb` as a fallback) |
| `settings.json` | `cargo clippy` as the on-save check, identical to CI |

Where "Run Task" lives — three equivalent routes, since the menu entry is not
always visible (it is under the **Terminal** menu, which is hidden when the menu
bar is collapsed):

| Route | How |
| --- | --- |
| Command Palette | `Ctrl-Shift-P`, type `Run Task`, choose **Tasks: Run Task**, then the task name |
| Keyboard | `Ctrl-Shift-B` runs the default *build* task (`cargo build`) directly |
| Menu bar | `Terminal → Run Task…` (press `Alt` first if the menu bar is hidden) |

If the list is empty, run **Developer: Reload Window** once: VS Code only reads
`tasks.json` when the folder is opened.

Breakpoints: set one in `src/sync.rs` (for example in `sync_host_lists`) and
press F5; the debugger stops before the first HTTP call.

`RUST_BACKTRACE=1 cargo run -- -p 0` is **not** a debugger command — it is how
you get a backtrace from a panic when running normally in a terminal. The
debugger equivalent is the `"env": { "RUST_BACKTRACE": "1" }` entry that every
configuration in `launch.json` already sets.

### Debugging without VS Code

`rust-gdb`/`rust-lldb` are thin wrappers: they take **only the binary**, so
program arguments must be given to the debugger, not to the wrapper.

```bash
# gdb, arguments on the command line
gdb --args target/debug/nginxy --config-dir . -p 0
(gdb) break src/sync.rs:303
(gdb) run

# or start gdb, then set the arguments
rust-gdb target/debug/nginxy
(gdb) set args --config-dir . -p 0
(gdb) run

# lldb needs the lldb package installed (sudo apt install lldb)
lldb target/debug/nginxy
(lldb) settings set target.run-args --config-dir . -p 0
(lldb) run
```


Static Linux binary:

```bash
rustup target add x86_64-unknown-linux-musl
sudo apt-get install musl-tools cmake
cargo build --release --target x86_64-unknown-linux-musl
```

## License

MIT — see [LICENSE](LICENSE).

## Why nginxy?

The use case is:

- You run servers behind a Cloudflare tunnel with a `cloudflared` daemon
- The tunnel connects to an Nginx Proxy Manager instance that forwards requests
  to web servers, cloud servers and so on
- You have 2 or more such servers, for High Availability and load balancing
- When you change the main server, the others must follow

`nginxy` reads the proxy hosts of the source and propagates the differences to
all targets.

### Author

John Zoetebier — [aip.transparent.co.nz](http://aip.transparent.co.nz)
