//! The sync loop: token refresh, source polling, per-target passes, retries.
//!
//! The phase structure is fixed — token refresh, source fetch,
//! change detection, per-target reconcile — because the behaviour operators see
//! (how often the source is polled, when a target is retried, what a dry run
//! prints) follows from it.

use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};

use crate::host::{self, ProxyHost};
use crate::http::Session;
use crate::npm::{self, ListError};
use crate::settings::{self, Auth, Settings};
use crate::stop::Stop;

/// NPM tokens live 24 h; refresh at 12 h so a long period cannot outlive one.
pub const AUTH_REFRESH_INTERVAL: Duration = Duration::from_secs(12 * 60 * 60);

/// Exit code for a run stopped with Ctrl-C, the shell convention for SIGINT.
pub const EXIT_INTERRUPTED: u8 = 130;

enum SyncError {
    /// A transport failure: the target is retried and the token dropped.
    Request(String),
    /// Anything else: reported, target retried.
    Other(String),
    /// The target was reachable, but individual create/update/delete calls
    /// failed. The target must stay in the retry list, otherwise a write that
    /// failed is only noticed by reading the log.
    Partial(Vec<String>),
}

pub struct Engine<'a> {
    settings: &'a Settings,
    period: i64,
    dry_run: bool,
    stop: Stop,
    sessions: HashMap<String, Session>,
    source_auth: Auth,
    target_auth: Vec<Auth>,
    /// Targets that still need a sync, keyed by URL.
    unsynced: BTreeSet<String>,
    previous_source_hosts: Option<Vec<ProxyHost>>,
    last_auth: Option<Instant>,
    auth_refresh: Duration,
}

impl<'a> Engine<'a> {
    pub fn new(
        settings: &'a Settings,
        period: i64,
        dry_run: bool,
        stop: Stop,
    ) -> Result<Self, reqwest::Error> {
        let mut sessions = HashMap::new();
        sessions.insert(
            settings.source.url.clone(),
            Session::new(&settings.source.url)?,
        );
        for target in &settings.targets {
            if !sessions.contains_key(&target.url) {
                sessions.insert(target.url.clone(), Session::new(&target.url)?);
            }
        }

        Ok(Self {
            settings,
            period,
            dry_run,
            stop,
            sessions,
            source_auth: settings::resolve_auth(
                settings.source.auth_payload.as_ref(),
                &settings.env,
            ),
            target_auth: settings
                .targets
                .iter()
                .map(|target| settings::resolve_auth(target.auth_payload.as_ref(), &settings.env))
                .collect(),
            unsynced: settings.targets.iter().map(|t| t.url.clone()).collect(),
            previous_source_hosts: None,
            last_auth: None,
            auth_refresh: AUTH_REFRESH_INTERVAL,
        })
    }

    /// Test hook: shorten the 12 h token refresh interval.
    pub fn with_auth_refresh(mut self, interval: Duration) -> Self {
        self.auth_refresh = interval;
        self
    }

    /// Targets that still need a sync, by URL.
    ///
    /// A target leaves this set only after a pass in which every write
    /// succeeded, so a failed create/update/delete keeps it here and the next
    /// cycle retries it. Exposed so tests can assert that without waiting for a
    /// second cycle, and so a status display could use it later.
    pub fn pending_targets(&self) -> Vec<&str> {
        self.unsynced.iter().map(String::as_str).collect()
    }

    /// Runs until `period <= 0` (one cycle) or a stop is requested.
    ///
    /// Returns [`EXIT_INTERRUPTED`] when Ctrl-C ended the run, otherwise `0`.
    pub fn run(&mut self) -> u8 {
        let mode = if self.dry_run { " (DRY RUN MODE)" } else { "" };
        log::info!(
            "Starting Sync{mode}: {} targets configured.",
            self.settings.targets.len()
        );

        let source_url = self.settings.source.url.clone();

        loop {
            if self.stop.is_stopped() {
                return EXIT_INTERRUPTED;
            }

            // Phase 0: drop tokens periodically so they cannot expire mid-cycle.
            self.refresh_tokens_if_due();

            // Lazy authentication, source first.
            if !self.session_has_token(&source_url) {
                self.authenticate(&source_url, &self.source_auth.clone());
            }
            if !self.session_has_token(&source_url) {
                log::error!("Source session not authenticated. Skipping cycle.");
                if self.period <= 0 {
                    break;
                }
                if self.sleep_period() {
                    return EXIT_INTERRUPTED;
                }
                continue;
            }

            // Phase 1: fetch the source's proxy hosts.
            let Some(source_hosts) = self.fetch_source_hosts() else {
                if self.period <= 0 {
                    break;
                }
                if self.sleep_period() {
                    return EXIT_INTERRUPTED;
                }
                continue;
            };

            // Phase 2: detect whether the source moved. Compared as configuration
            // only, so a host that was merely saved (`modified_on` bumped) does
            // not schedule every target again.
            let source_changed = self
                .previous_source_hosts
                .as_ref()
                .map(|previous| host::configuration_of(previous))
                != Some(host::configuration_of(&source_hosts));
            if source_changed {
                // A change on the source schedules every target again.
                self.unsynced
                    .extend(self.settings.targets.iter().map(|t| t.url.clone()));

                if self.previous_source_hosts.is_none() {
                    log::info!(
                        "Initial sync: Source has {} proxy hosts.",
                        source_hosts.len()
                    );
                } else {
                    log::info!("Source change detected ({} hosts).", source_hosts.len());
                }

                // A dry run must not consume the change, or the next cycle would
                // report "no changes" although nothing was written.
                if !self.dry_run {
                    self.previous_source_hosts = Some(source_hosts.clone());
                }
            }

            // Phase 3: sync the targets that need it.
            if self.period > 0 && !source_changed && self.unsynced.is_empty() {
                log::info!("No changes in source and all targets are in sync.");
                log::info!("Next sync in {} seconds.", self.period);
                if self.sleep_period() {
                    return EXIT_INTERRUPTED;
                }
                continue;
            }

            if self.period <= 0 {
                log::info!("One-time sync mode. Proceeding with sync to targets.");
            } else if !self.unsynced.is_empty() && !source_changed {
                log::info!(
                    "Retrying sync for {} unsynced target(s).",
                    self.unsynced.len()
                );
            }

            self.sync_targets(&source_hosts);

            if self.stop.is_stopped() {
                return EXIT_INTERRUPTED;
            }

            if self.period <= 0 {
                log::info!("One-time sync completed. Exiting.");
                break;
            }

            log::info!("Sync completed. Next sync in {} seconds.", self.period);
            if self.sleep_period() {
                return EXIT_INTERRUPTED;
            }
        }

        0
    }

    fn refresh_tokens_if_due(&mut self) {
        let due = match self.last_auth {
            None => true,
            Some(at) => at.elapsed() > self.auth_refresh,
        };
        if !due {
            return;
        }
        // The first cycle has never authenticated, so it logs this line too:
        // `None` means "refresh now".
        log::info!("Token refresh interval reached. Invalidating sessions.");
        for session in self.sessions.values_mut() {
            session.invalidate();
        }
        self.last_auth = Some(Instant::now());
    }

    fn fetch_source_hosts(&mut self) -> Option<Vec<ProxyHost>> {
        let url = self.settings.source.url.clone();
        let endpoint = format!("{url}/nginx/proxy-hosts");
        log::info!("Fetching proxy hosts from {endpoint}");

        let session = self.sessions.get_mut(&url)?;
        match npm::list_proxy_hosts(session) {
            Ok(hosts) => Some(hosts),
            Err(ListError::Status(code)) => {
                log::error!("Failed to fetch proxy hosts from source: {code}");
                None
            }
            Err(ListError::Transport(message)) => {
                log::error!("Connection error to source: {message}");
                session.invalidate();
                None
            }
            Err(ListError::Decode(message)) => {
                log::error!("Failed to parse proxy hosts from source: {message}");
                None
            }
        }
    }

    fn sync_targets(&mut self, source_hosts: &[ProxyHost]) {
        let targets = self.settings.targets.clone();

        for (index, target) in targets.iter().enumerate() {
            if self.stop.is_stopped() {
                return;
            }

            let url = target.url.clone();
            if !self.unsynced.contains(&url) {
                continue;
            }

            log::info!(
                "--- Syncing to Target: {} ({url}) ---",
                settings::target_name(target)
            );

            if !self.session_has_token(&url) {
                let auth = self
                    .target_auth
                    .get(index)
                    .cloned()
                    .unwrap_or(Auth::Missing);
                self.authenticate(&url, &auth);
            }

            if self.session_has_token(&url) {
                let success = self.sync_to_target(source_hosts, &url);
                // Dry runs never clear the flag: nothing was written, so the
                // target is still out of date.
                if success && !self.dry_run {
                    self.unsynced.remove(&url);
                }
            } else {
                log::warn!(
                    "Skipping target {} - Authentication failed.",
                    settings::target_name(target)
                );
            }
        }
    }

    /// One target, one pass: every failure becomes a log line and a retry.
    fn sync_to_target(&mut self, source_hosts: &[ProxyHost], target_url: &str) -> bool {
        match self.sync_host_lists(source_hosts, target_url) {
            Ok(()) => true,
            Err(SyncError::Request(message)) => {
                log::error!("Network error during sync to target {target_url}: {message}");
                if let Some(session) = self.sessions.get_mut(target_url) {
                    session.invalidate();
                }
                false
            }
            Err(SyncError::Other(message)) => {
                log::error!("An error occurred during sync: {message}");
                false
            }
            Err(SyncError::Partial(failures)) => {
                // Returning false is the whole point: sync_targets() keeps the
                // target in `unsynced`, so the next cycle tries again instead of
                // waiting for the next source change.
                log::error!(
                    "{} operation(s) failed on target {target_url}; it stays in the retry list: {}",
                    failures.len(),
                    failures.join(", ")
                );
                false
            }
        }
    }
    /// The body of a pass: read the target, delete what the source no longer
    /// has, then create or update the rest.
    fn sync_host_lists(
        &mut self,
        source_hosts: &[ProxyHost],
        target_url: &str,
    ) -> Result<(), SyncError> {
        // Writes that failed. An empty list at the end means the target really
        // is in step; anything in it keeps the target in the retry list.
        let mut failures: Vec<String> = Vec::new();

        let target_hosts = {
            let session = self.session_mut(target_url)?;
            npm::list_proxy_hosts(session).map_err(|err| match err {
                // A non-2xx answer counts as a request failure, so the token
                // is discarded as it is on a transport error.
                ListError::Status(code) => SyncError::Request(format!("HTTP {code}")),
                ListError::Transport(message) => SyncError::Request(message),
                ListError::Decode(message) => SyncError::Other(message),
            })?
        };

        log::info!(
            "Found {} hosts on source and {} on target.",
            source_hosts.len(),
            target_hosts.len()
        );

        let (source_keys, source_by_key) = host::index_by_key(source_hosts);
        let (target_keys, target_by_key) = host::index_by_key(&target_hosts);

        // Phase 1: delete hosts whose domain set the source no longer has. Doing
        // this first frees domain names that the create pass may reuse.
        for key in &target_keys {
            if source_by_key.contains_key(key) {
                continue;
            }
            let target_host = target_by_key[key];

            if self.dry_run {
                log::info!(
                    "[DRY RUN] Would delete host: {:?}",
                    target_host.domain_names
                );
                continue;
            }

            log::info!(
                "Host with domains {:?} no longer exists as a set in source. Deleting.",
                target_host.domain_names
            );

            let Some(id) = target_host.id else {
                log::error!(
                    "Host with domains {:?} has no id on {target_url}; cannot delete it.",
                    target_host.domain_names
                );
                failures.push(format!("delete {:?} (no id)", target_host.domain_names));
                continue;
            };

            let session = self.session_mut(target_url)?;
            if !npm::delete_proxy_host(session, id, Some(target_host)) {
                failures.push(format!("delete {:?}", target_host.domain_names));
            }
        }

        // Phase 2: create or update.
        for key in &source_keys {
            let source_host = source_by_key[key];

            match target_by_key.get(key) {
                Some(target_host) => {
                    // Per-instance fields are never copied, but a difference is
                    // worth a line: someone added a certificate or an access
                    // list on the source and the target does not have it.
                    for field in host::not_synchronised_differences(source_host, target_host) {
                        log::info!(
                            "Host {:?}: {field} differs on the target and is not synchronised (per-instance value).",
                            source_host.domain_names
                        );
                    }

                    if host::should_update(source_host, target_host) {
                        if self.dry_run {
                            log::info!(
                                "[DRY RUN] Would update host: {:?}",
                                source_host.domain_names
                            );
                            continue;
                        }

                        let Some(id) = target_host.id else {
                            log::error!(
                                "Host with domains {:?} has no id on {target_url}; cannot update it.",
                                source_host.domain_names
                            );
                            failures.push(format!("update {:?} (no id)", source_host.domain_names));
                            continue;
                        };

                        log::info!(
                            "Updating host with domains {:?} (Target ID: {id})",
                            source_host.domain_names
                        );
                        let session = self.session_mut(target_url)?;
                        if npm::update_proxy_host(session, id, source_host).is_none() {
                            failures.push(format!("update {:?}", source_host.domain_names));
                        }
                    } else {
                        log::info!(
                            "Host {:?} is already up to date on target.",
                            source_host.domain_names
                        );
                    }
                }
                None => {
                    if self.dry_run {
                        log::info!(
                            "[DRY RUN] Would create host: {:?}",
                            source_host.domain_names
                        );
                        continue;
                    }

                    log::info!(
                        "Creating host with domains {:?} on target.",
                        source_host.domain_names
                    );
                    let session = self.session_mut(target_url)?;
                    if npm::add_proxy_host(session, source_host).is_none() {
                        failures.push(format!("create {:?}", source_host.domain_names));
                    }
                }
            }
        }

        if failures.is_empty() {
            Ok(())
        } else {
            Err(SyncError::Partial(failures))
        }
    }

    fn session_mut(&mut self, url: &str) -> Result<&mut Session, SyncError> {
        self.sessions
            .get_mut(url)
            .ok_or_else(|| SyncError::Other(format!("no session for {url}")))
    }

    fn session_has_token(&self, url: &str) -> bool {
        self.sessions
            .get(url)
            .map(Session::has_token)
            .unwrap_or(false)
    }

    fn authenticate(&mut self, url: &str, auth: &Auth) {
        if let Some(session) = self.sessions.get_mut(url) {
            session.authenticate(auth);
        }
    }

    /// Sleeps one period; `true` when Ctrl-C cut it short.
    fn sleep_period(&self) -> bool {
        if self.period <= 0 {
            return self.stop.is_stopped();
        }
        self.stop.sleep(Duration::from_secs(self.period as u64))
    }
}
