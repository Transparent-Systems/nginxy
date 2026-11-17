//! nginxy — synchronise Nginx Proxy Manager proxy hosts.
//!
//! This crate is a Rust port of the Python `nginx-sync` application. It keeps
//! the Python program's observable behaviour: the same configuration files, the
//! same log file and line format, the same NPM API calls in the same order, the
//! same exit codes. See `PLAN.md` §2 ("Behavioural contract") for the list that
//! the tests here are written against.
//!
//! Module map (mirrors the Python functions they replace):
//!
//! | module      | replaces                                             |
//! |-------------|------------------------------------------------------|
//! | [`cli`]     | `parse_args()`, `run()`, `cli()`                     |
//! | [`settings`]| `load_config()`, `load_settings()`, `get_auth_payload()` |
//! | [`logging`] | `setup_logging()`, `RotatingFileHandler`             |
//! | [`http`]    | `requests.Session`, `describe_http_failure()`        |
//! | [`host`]    | `should_update()`, payload filtering, domain keys    |
//! | [`npm`]     | `get_token()`, `add/update/delete_proxy_host()`      |
//! | [`sync`]    | `main()`, `sync_to_target()`                         |
//! | [`template`]| `${VAR}` expansion (an addition, not in the Python app) |
//! | [`version`] | `read_version()`                                     |

pub mod cli;
pub mod host;
pub mod http;
pub mod logging;
pub mod npm;
pub mod settings;
pub mod stop;
pub mod sync;
pub mod template;
pub mod version;
