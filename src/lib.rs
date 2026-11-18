//! nginxy — synchronise Nginx Proxy Manager proxy hosts.
//!
//! One source instance is read and every target instance is made to match it:
//! fixed configuration files, one log format, the NPM API calls in a defined
//! order, and exit codes a service manager can act on. `doc/PLAN.md` Section 2
//! ("Behavioural contract") is the list the tests here are written against.
//!
//! Module map:
//!
//! | module       | responsibility                                        |
//! |--------------|-------------------------------------------------------|
//! | [`cli`]      | arguments, level precedence, exit codes, Ctrl-C        |
//! | [`settings`] | config folder, YAML models, `.env`, auth resolution    |
//! | [`logging`]  | record format, rotating file, stderr                   |
//! | [`http`]     | blocking client and bearer token per endpoint          |
//! | [`host`]     | proxy-host model, payload filtering, domain keys       |
//! | [`npm`]      | token, list, create, update, delete                    |
//! | [`sync`]     | the loop: phases, retry set, dry run                   |
//! | [`stop`]     | interruptible sleep                                    |
//! | [`template`] | `${VAR}` expansion in parsed configuration values      |
//! | [`version`]  | version resolution for `--version`                     |

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
