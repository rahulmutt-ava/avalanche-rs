// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Health worker + checker registry (mirror Go `api/health`; specs 12 §3.4,
//! 14 §5, 17 §2.2 #21, 18 §2.13).
//!
//! [`Health`] owns **three independent check sets** — readiness, health,
//! liveness — exactly like Go's `health.health` (`api/health/health.go`):
//!
//! - **readiness** checks register through the *monotonic* wrapper
//!   (`RegisterMonotonicCheck`): once a readiness check passes with non-null
//!   details, its result is cached and the underlying checker never runs again.
//! - **health** / **liveness** checks re-run on every interval tick.
//!
//! [`Health::start`] spawns one worker loop per set; each runs its checks
//! immediately and then on every `health-check-frequency` tick (17 §2.2 #21).
//! Results carry `contiguousFailures` / `timeOfFirstFailure` streak tracking
//! (`api/health/worker.go::runCheck`). A panicking checker never kills its
//! worker loop: the panic is contained and reported as a failing result with
//! the stable message `health check panicked: <payload>`.
//!
//! **Metrics (18 §2.13).** The shared `checks_failing` gauge (labels
//! `check` = worker name, `tag`) registers against the *caller-provided* plain
//! [`prometheus::Registry`]; the `avalanche_health` namespace prefix is applied
//! by the gatherer tree at node assembly (M8.21/M8.29), mirroring Go's
//! `metrics.MakeAndRegister(gatherer, "avalanche_health")`.
//!
//! **Spec note (12 §3.4 correction).** Go's `api/health` worker does **no**
//! EWMA averaging: `health-check-averager-halflife` configures the *checkers*
//! that use running averages (the router health config and the network
//! send-fail-rate meters — `config/config.go`), not this worker. The worker is
//! a plain ticker loop, reproduced faithfully here.
//!
//! **Node wiring notes (M8.29, nothing to wire here):** the health service is
//! initialized **before** the chain manager (12 §2.2 step 18) so chains can
//! register their bootstrap-health checks, and node shutdown registers a
//! `shuttingDown` health check that fails with `"server shutting down"`
//! (Go `node/node.go::Shutdown`).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use chrono::Utc;
use futures::FutureExt;
use futures::future::BoxFuture;
use parking_lot::{Mutex, RwLock};
use prometheus::IntGaugeVec;
use serde_json::Value;
use tokio::task::JoinHandle;
use tracing::{info, warn};

pub mod handler;
pub mod types;

pub use handler::handler;
pub use types::{APIArgs, APIReply};

/// The metric label differentiating the check sets (Go `health.CheckLabel`).
/// Its value is the worker name: `"readiness"`, `"health"`, or `"liveness"`.
pub const CHECK_LABEL: &str = "check";
/// The metric label differentiating check tags (Go `health.TagLabel`).
pub const TAG_LABEL: &str = "tag";
/// Automatically added to every registered check (Go `health.AllTag`).
/// Registering a check *with* this tag is rejected (it would double count).
pub const ALL_TAG: &str = "all";
/// Checks registered with this tag act as if they specified every registered
/// tag: they are always included in every query result (Go
/// `health.ApplicationTag`).
pub const APPLICATION_TAG: &str = "application";

/// Errors from registering health checks (mirror `api/health/worker.go`).
#[derive(Debug, thiserror::Error)]
pub enum HealthError {
    /// The reserved [`ALL_TAG`] was supplied as a check tag (Go
    /// `errRestrictedTag`).
    #[error("restricted tag: {0:?}")]
    RestrictedTag(String),
    /// A check with this name is already registered on the set (Go
    /// `errDuplicateCheck`).
    #[error("duplicate check: {0:?}")]
    DuplicateCheck(String),
    /// Registering the `checks_failing` gauge failed.
    #[error(transparent)]
    Metrics(#[from] prometheus::Error),
}

/// The error a failing [`Checker`] reports. Mirrors the Go convention where
/// `HealthCheck` returns `(details, err)`: the message is the `err.Error()`
/// string surfaced in [`types::Result::error`], and optional details ride
/// along into [`types::Result::details`] (Go checkers may return both).
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct CheckError {
    /// The failure message (Go `err.Error()`).
    message: String,
    /// Details reported alongside the failure (Go's non-nil `details` with a
    /// non-nil `err`).
    details: Option<Value>,
}

impl CheckError {
    /// A failure with the given message and no details.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            details: None,
        }
    }

    /// Attaches details reported alongside the failure.
    #[must_use]
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }
}

/// What a single run of a [`Checker`] yields: JSON-marshallable details on
/// success (Go `(interface{}, nil)`; `Value::Null` ≙ Go `nil` details), or a
/// [`CheckError`] on failure.
pub type CheckResult = std::result::Result<Value, CheckError>;

/// A component whose health can be checked (mirror Go `health.Checker`).
///
/// Go's `HealthCheck(context.Context)` context parameter is dropped: the Rust
/// worker stops by awaiting task completion, and checkers are expected to be
/// internally bounded. Any `Fn() -> BoxFuture<'static, CheckResult> + Send +
/// Sync` closure is a `Checker` (the `CheckerFunc` equivalent).
pub trait Checker: Send + Sync {
    /// Runs the health check, returning details and, if unhealthy, an error.
    fn health_check(&self) -> BoxFuture<'static, CheckResult>;
}

impl<F> Checker for F
where
    F: Fn() -> BoxFuture<'static, CheckResult> + Send + Sync,
{
    fn health_check(&self) -> BoxFuture<'static, CheckResult> {
        self()
    }
}

/// A registered checker plus its registration-time tag metadata (Go
/// `taggedChecker`).
struct TaggedChecker {
    /// The checker to run.
    checker: Arc<dyn Checker>,
    /// Whether the check was registered with [`APPLICATION_TAG`].
    is_application_check: bool,
    /// The tags supplied at registration (excluding the implicit [`ALL_TAG`]).
    tags: Vec<String>,
}

/// The mutable state of one check set, behind the worker's lock.
#[derive(Default)]
struct WorkerState {
    /// Registered checkers by name.
    checks: HashMap<String, Arc<TaggedChecker>>,
    /// Latest result per check; a freshly registered check holds
    /// [`types::Result::not_yet_run`].
    results: HashMap<String, types::Result>,
    /// tag -> set of check names (always includes [`ALL_TAG`] entries).
    tags: HashMap<String, HashSet<String>>,
    /// The number of currently failing [`APPLICATION_TAG`] checks; folded into
    /// a tag's gauge when the tag is first registered (Go
    /// `numFailingApplicationChecks`).
    num_failing_application_checks: i64,
}

/// One named check set + its run loop state (Go `api/health/worker.go`).
struct Worker {
    /// `"readiness"` / `"health"` / `"liveness"` — the [`CHECK_LABEL`] value.
    name: &'static str,
    /// The shared `checks_failing` gauge (18 §2.13).
    failing_checks: IntGaugeVec,
    /// Registered checks, results, and tag indices.
    state: RwLock<WorkerState>,
}

impl Worker {
    /// A new worker; pre-sets the (`name`, `all`) and (`name`, `application`)
    /// gauge series to 0 (Go `newWorker`).
    fn new(name: &'static str, failing_checks: IntGaugeVec) -> Self {
        for tag in [ALL_TAG, APPLICATION_TAG] {
            failing_checks.with_label_values(&[name, tag]).set(0);
        }
        Self {
            name,
            failing_checks,
            state: RwLock::new(WorkerState::default()),
        }
    }

    /// Registers a check under `name` (Go `worker.RegisterCheck`): rejects the
    /// reserved [`ALL_TAG`] and duplicates, indexes the tags, seeds the result
    /// with "not yet run", and counts the new check as failing in the metrics.
    fn register_check(
        &self,
        name: String,
        checker: Arc<dyn Checker>,
        tags: &[String],
    ) -> std::result::Result<(), HealthError> {
        // [ALL_TAG] in [tags] would make the metrics double count.
        if tags.iter().any(|t| t == ALL_TAG) {
            return Err(HealthError::RestrictedTag(ALL_TAG.to_string()));
        }

        let mut state = self.state.write();
        if state.checks.contains_key(&name) {
            return Err(HealthError::DuplicateCheck(name));
        }

        // Add the check to each tag, plus the special AllTag descriptor.
        for tag in tags.iter().map(String::as_str).chain([ALL_TAG]) {
            state
                .tags
                .entry(tag.to_string())
                .or_default()
                .insert(name.clone());
        }

        let is_application_check = state
            .tags
            .get(APPLICATION_TAG)
            .is_some_and(|names| names.contains(&name));
        let tc = Arc::new(TaggedChecker {
            checker,
            is_application_check,
            tags: tags.to_vec(),
        });
        state.checks.insert(name.clone(), Arc::clone(&tc));
        state
            .results
            .insert(name.clone(), types::Result::not_yet_run());

        // Whenever a new check is added - it is failing.
        info!(
            worker = self.name,
            check = %name,
            ?tags,
            "registered new check and initialized its state to failing"
        );
        // healthy = false, register = true.
        self.update_metrics(&mut state, &tc, false, true);
        Ok(())
    }

    /// Registers a *monotonic* check (Go `worker.RegisterMonotonicCheck`):
    /// once the wrapped checker passes with non-null details, the details are
    /// cached and the checker never runs again. (Go caches via
    /// `utils.Atomic[any]` and short-circuits on non-nil details, so a check
    /// passing with nil details keeps re-running; mirrored exactly.)
    fn register_monotonic_check(
        &self,
        name: String,
        checker: Arc<dyn Checker>,
        tags: &[String],
    ) -> std::result::Result<(), HealthError> {
        let cached: Arc<RwLock<Option<Value>>> = Arc::new(RwLock::new(None));
        let wrapped = move || -> BoxFuture<'static, CheckResult> {
            let cached = Arc::clone(&cached);
            let checker = Arc::clone(&checker);
            Box::pin(async move {
                if let Some(details) = cached.read().clone() {
                    return Ok(details);
                }
                let result = checker.health_check().await;
                if let Ok(details) = &result
                    && !details.is_null()
                {
                    *cached.write() = Some(details.clone());
                }
                result
            })
        };
        self.register_check(name, Arc::new(wrapped), tags)
    }

    /// The current results filtered by `tags` (Go `worker.Results`): no tags ≙
    /// [`ALL_TAG`] (all checks); [`APPLICATION_TAG`] is always included; the
    /// report is healthy iff every included result has no error.
    fn results(&self, tags: &[String]) -> (BTreeMap<String, types::Result>, bool) {
        let state = self.state.read();

        // If no tags are specified, return all checks.
        let all_tag = [ALL_TAG.to_string()];
        let tags = if tags.is_empty() { &all_tag[..] } else { tags };

        let mut tag_set: HashSet<&str> = tags.iter().map(String::as_str).collect();
        // We always want to include the application tag.
        tag_set.insert(APPLICATION_TAG);

        let mut names: HashSet<&String> = HashSet::new();
        for tag in tag_set {
            if let Some(set) = state.tags.get(tag) {
                names.extend(set);
            }
        }

        let mut results = BTreeMap::new();
        let mut healthy = true;
        for name in names {
            if let Some(result) = state.results.get(name) {
                healthy = healthy && result.error.is_none();
                results.insert(name.clone(), result.clone());
            }
        }
        (results, healthy)
    }

    /// Runs every registered check once, concurrently, and waits for all of
    /// them (Go `worker.runChecks`). Checks registered mid-run are picked up
    /// on the next iteration.
    async fn run_checks(&self) {
        // Snapshot the checks; no locks are held while checkers run, so a
        // checker may itself call `register_*_check` without deadlocking.
        let checks: Vec<(String, Arc<TaggedChecker>)> = {
            let state = self.state.read();
            state
                .checks
                .iter()
                .map(|(name, tc)| (name.clone(), Arc::clone(tc)))
                .collect()
        };

        futures::future::join_all(
            checks
                .into_iter()
                .map(|(name, check)| self.run_check(name, check)),
        )
        .await;
    }

    /// Runs one check and folds the outcome into the results map, maintaining
    /// the `contiguousFailures` / `timeOfFirstFailure` streak and the
    /// transition logs + metrics (Go `worker.runCheck`).
    ///
    /// **Panic containment.** A panicking checker is converted into a failing
    /// result with the stable message `health check panicked: <payload>` (no
    /// Go parity string exists — a Go checker cannot kill the worker this
    /// way), so the worker loop keeps running and the check reports unhealthy
    /// with a growing `contiguousFailures` streak instead of freezing a
    /// possibly-stale `healthy: true`.
    async fn run_check(&self, name: String, check: Arc<TaggedChecker>) {
        let start = Instant::now();
        // No locks are held while the checker runs (see `run_checks`). The
        // async block defers the `health_check()` call into the polled future
        // so `catch_unwind` contains panics both from constructing the future
        // and from awaiting it.
        let checker = Arc::clone(&check.checker);
        let outcome = AssertUnwindSafe(async move { checker.health_check().await })
            .catch_unwind()
            .await
            .unwrap_or_else(|payload| {
                Err(CheckError::new(format!(
                    "health check panicked: {}",
                    panic_message(payload.as_ref())
                )))
            });
        let end = Utc::now();

        let mut result = types::Result {
            timestamp: end,
            duration: start.elapsed(),
            ..types::Result::default()
        };

        let mut state = self.state.write();
        let prev = state.results.get(&name).cloned().unwrap_or_default();
        match outcome {
            Err(e) => {
                result.details = e.details.clone();
                result.error = Some(e.message.clone());
                result.contiguous_failures = prev.contiguous_failures.saturating_add(1);
                result.time_of_first_failure = if prev.contiguous_failures > 0 {
                    prev.time_of_first_failure
                } else {
                    Some(end)
                };
                if prev.error.is_none() {
                    warn!(
                        worker = self.name,
                        check = %name,
                        tags = ?check.tags,
                        error = %e,
                        "check started failing"
                    );
                    self.update_metrics(&mut state, &check, false, false);
                }
            }
            Ok(details) => {
                // Go `nil` details (Value::Null) are omitted from the JSON.
                if !details.is_null() {
                    result.details = Some(details);
                }
                if prev.error.is_some() {
                    info!(
                        worker = self.name,
                        check = %name,
                        tags = ?check.tags,
                        "check started passing"
                    );
                    self.update_metrics(&mut state, &check, true, false);
                }
            }
        }
        state.results.insert(name, result);
    }

    /// Adjusts the `checks_failing` gauge for a check transition (Go
    /// `worker.updateMetrics`). `healthy` decrements, unhealthy increments;
    /// `register` must be true only on first registration (it folds the
    /// currently failing application checks into a brand-new tag's series).
    fn update_metrics(
        &self,
        state: &mut WorkerState,
        tc: &TaggedChecker,
        healthy: bool,
        register: bool,
    ) {
        if tc.is_application_check {
            // An application check counts against every registered tag
            // (state.tags includes ALL_TAG).
            for tag in state.tags.keys() {
                let gauge = self.failing_checks.with_label_values(&[self.name, tag]);
                if healthy {
                    gauge.dec();
                } else {
                    gauge.inc();
                }
            }
            state.num_failing_application_checks = if healthy {
                state.num_failing_application_checks.saturating_sub(1)
            } else {
                state.num_failing_application_checks.saturating_add(1)
            };
        } else {
            for tag in &tc.tags {
                let gauge = self.failing_checks.with_label_values(&[self.name, tag]);
                if healthy {
                    gauge.dec();
                } else {
                    gauge.inc();
                    // If this registration created the tag, fold in the
                    // currently failing application-wide checks.
                    if register && state.tags.get(tag).map(HashSet::len) == Some(1) {
                        gauge.add(state.num_failing_application_checks);
                    }
                }
            }
            let gauge = self.failing_checks.with_label_values(&[self.name, ALL_TAG]);
            if healthy {
                gauge.dec();
            } else {
                gauge.inc();
            }
        }
    }
}

/// Renders a checker's panic payload as the `<payload>` part of the stable
/// `health check panicked: <payload>` failure message: `&str` / `String`
/// payloads (everything `panic!` with a message produces) verbatim, anything
/// else a placeholder.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("<non-string panic payload>")
}

/// The health service: three check sets + their periodic worker loops (mirror
/// Go `health.health`; specs 12 §3.4, 17 §2.2 #21). See the module docs for
/// the readiness-monotonic and tag semantics.
pub struct Health {
    /// Readiness checks (monotonic: cached once passing).
    readiness: Arc<Worker>,
    /// Health checks (re-run every tick).
    health: Arc<Worker>,
    /// Liveness checks (re-run every tick).
    liveness: Arc<Worker>,
    /// Whether [`Health::start`] already ran (Go `startOnce`).
    started: AtomicBool,
    /// Stop signal for the worker loops (flipped to `true` by
    /// [`Health::stop`]).
    stop_tx: tokio::sync::watch::Sender<bool>,
    /// The spawned worker loops, awaited by [`Health::stop`].
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl Health {
    /// Creates the health service, registering the `checks_failing` gauge
    /// (labels [`CHECK_LABEL`], [`TAG_LABEL`]; 18 §2.13) on `registerer` —
    /// a plain registry; the `avalanche_health` namespace is applied by the
    /// gatherer tree at node assembly (M8.21/M8.29).
    pub fn new(registerer: &prometheus::Registry) -> std::result::Result<Self, HealthError> {
        let failing_checks = IntGaugeVec::new(
            prometheus::Opts::new(
                "checks_failing",
                "number of currently failing health checks",
            ),
            &[CHECK_LABEL, TAG_LABEL],
        )?;
        registerer.register(Box::new(failing_checks.clone()))?;
        let (stop_tx, _) = tokio::sync::watch::channel(false);
        Ok(Self {
            readiness: Arc::new(Worker::new("readiness", failing_checks.clone())),
            health: Arc::new(Worker::new("health", failing_checks.clone())),
            liveness: Arc::new(Worker::new("liveness", failing_checks)),
            started: AtomicBool::new(false),
            stop_tx,
            tasks: Mutex::new(Vec::new()),
        })
    }

    /// Registers a readiness check (monotonic; Go `RegisterReadinessCheck`).
    pub fn register_readiness_check(
        &self,
        name: impl Into<String>,
        checker: Arc<dyn Checker>,
        tags: &[String],
    ) -> std::result::Result<(), HealthError> {
        self.readiness
            .register_monotonic_check(name.into(), checker, tags)
    }

    /// Registers a health check (Go `RegisterHealthCheck`).
    pub fn register_health_check(
        &self,
        name: impl Into<String>,
        checker: Arc<dyn Checker>,
        tags: &[String],
    ) -> std::result::Result<(), HealthError> {
        self.health.register_check(name.into(), checker, tags)
    }

    /// Registers a liveness check (Go `RegisterLivenessCheck`).
    pub fn register_liveness_check(
        &self,
        name: impl Into<String>,
        checker: Arc<dyn Checker>,
        tags: &[String],
    ) -> std::result::Result<(), HealthError> {
        self.liveness.register_check(name.into(), checker, tags)
    }

    /// The readiness report filtered by `tags` (Go `Reporter.Readiness`):
    /// whether the node has finished initializing.
    #[must_use]
    pub fn readiness(&self, tags: &[String]) -> (BTreeMap<String, types::Result>, bool) {
        let (results, healthy) = self.readiness.results(tags);
        if !healthy {
            warn!(namespace = "readiness", reason = ?results, "failing check");
        }
        (results, healthy)
    }

    /// The health report filtered by `tags` (Go `Reporter.Health`): a
    /// summation of the node's health.
    #[must_use]
    pub fn health(&self, tags: &[String]) -> (BTreeMap<String, types::Result>, bool) {
        let (results, healthy) = self.health.results(tags);
        if !healthy {
            warn!(namespace = "health", reason = ?results, "failing check");
        }
        (results, healthy)
    }

    /// The liveness report filtered by `tags` (Go `Reporter.Liveness`):
    /// whether the node needs a restart.
    #[must_use]
    pub fn liveness(&self, tags: &[String]) -> (BTreeMap<String, types::Result>, bool) {
        let (results, healthy) = self.liveness.results(tags);
        if !healthy {
            warn!(namespace = "liveness", reason = ?results, "failing check");
        }
        (results, healthy)
    }

    /// Starts the periodic worker loops: each set runs its checks immediately
    /// and then on every `freq` tick (`health-check-frequency`; 17 §2.2 #21).
    /// Repeated calls are no-ops (Go `startOnce`). `freq` must be non-zero
    /// (the node config validates `health-check-frequency > 0`).
    pub fn start(&self, freq: Duration) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        let mut tasks = self.tasks.lock();
        for worker in [&self.readiness, &self.health, &self.liveness] {
            let worker = Arc::clone(worker);
            let mut stop_rx = self.stop_tx.subscribe();
            tasks.push(tokio::spawn(async move {
                let mut ticker = tokio::time::interval(freq);
                // Go's time.Ticker drops ticks missed while a run is in
                // flight; it never bursts to catch up.
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    tokio::select! {
                        // The first tick completes immediately, mirroring Go's
                        // runChecks-before-loop.
                        _ = ticker.tick() => worker.run_checks().await,
                        _ = stop_rx.changed() => return,
                    }
                }
            }));
        }
    }

    /// Stops the worker loops and waits for them to exit; no checks run after
    /// `stop` returns (Go `Stop`). Safe to call repeatedly.
    pub async fn stop(&self) {
        // Ignore the no-receivers error: stop before start is a no-op.
        let _ = self.stop_tx.send(true);
        let tasks: Vec<JoinHandle<()>> = std::mem::take(&mut *self.tasks.lock());
        for task in tasks {
            // A worker task neither panics nor is aborted; a join error here
            // only means the runtime is shutting down.
            let _ = task.await;
        }
    }

    /// Runs every registered check on all three sets once and waits for them.
    ///
    /// Not part of Go's `health.Health` interface (Go's `Start` covers the
    /// immediate first run); exposed for deterministic tests and bootstrap
    /// probes that cannot wait for a tick.
    ///
    /// **Test/bootstrap-only: must not be called after [`Health::start`]** —
    /// it is not serialized with the worker loops, so a concurrent run would
    /// race them over the shared results and failure streaks (guarded by a
    /// `debug_assert!`).
    pub async fn run_checks_now(&self) {
        debug_assert!(
            !self.started.load(Ordering::SeqCst),
            "run_checks_now must not be called after start()"
        );
        futures::future::join3(
            self.readiness.run_checks(),
            self.health.run_checks(),
            self.liveness.run_checks(),
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use assert_matches::assert_matches;
    use futures::future::BoxFuture;
    use pretty_assertions::assert_eq;
    use prometheus::Registry;
    use serde_json::{Value, json};

    use super::{ALL_TAG, APPLICATION_TAG, CheckResult, Checker, Health, HealthError};

    fn new_health() -> (Arc<Health>, Registry) {
        let registry = Registry::new();
        let health = Arc::new(Health::new(&registry).expect("Health::new()"));
        (health, registry)
    }

    /// A checker that always passes with the given details and counts its
    /// invocations.
    fn counting(details: Value) -> (Arc<dyn Checker>, Arc<AtomicU64>) {
        let runs = Arc::new(AtomicU64::new(0));
        let checker = {
            let runs = Arc::clone(&runs);
            Arc::new(move || -> BoxFuture<'static, CheckResult> {
                let details = details.clone();
                let runs = Arc::clone(&runs);
                Box::pin(async move {
                    runs.fetch_add(1, Ordering::SeqCst);
                    Ok(details)
                })
            }) as Arc<dyn Checker>
        };
        (checker, runs)
    }

    /// The current `checks_failing{check, tag}` gauge value in `registry`.
    fn gauge_value(registry: &Registry, check: &str, tag: &str) -> f64 {
        for family in registry.gather() {
            if family.get_name() != "checks_failing" {
                continue;
            }
            for metric in family.get_metric() {
                let matches = |name: &str, value: &str| {
                    metric
                        .get_label()
                        .iter()
                        .any(|l| l.get_name() == name && l.get_value() == value)
                };
                if matches(super::CHECK_LABEL, check) && matches(super::TAG_LABEL, tag) {
                    return metric.get_gauge().get_value();
                }
            }
        }
        panic!("checks_failing series (check={check:?}, tag={tag:?}) not found");
    }

    // ------------------------------------------------------------------
    // Panic containment: a panicking checker (a) marks the check unhealthy
    // with the stable `health check panicked: <payload>` message and a
    // growing contiguousFailures streak, and (b) does not kill the worker
    // loop — a sibling check keeps updating on later ticks.
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn panicking_checker_fails_without_killing_worker() {
        let (health, _registry) = new_health();
        let panicking = Arc::new(|| -> BoxFuture<'static, CheckResult> {
            Box::pin(async { panic!("kaboom") })
        }) as Arc<dyn Checker>;
        health
            .register_health_check("panicky", panicking, &[])
            .expect("register_health_check(panicky)");
        let (sibling, sibling_runs) = counting(Value::Null);
        health
            .register_health_check("sibling", sibling, &[])
            .expect("register_health_check(sibling)");

        health.start(Duration::from_millis(10));

        // The sibling keeps updating across >= 3 ticks even though `panicky`
        // panics on every one — the worker loop survived the panics.
        tokio::time::timeout(Duration::from_secs(5), async {
            while sibling_runs.load(Ordering::SeqCst) < 3 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("sibling check kept running after panics");

        let (checks, healthy) = health.health(&[]);
        assert!(!healthy, "panicking check must report unhealthy");
        let panicky = checks.get("panicky").expect("panicky result");
        assert_eq!(
            panicky.error.as_deref(),
            Some("health check panicked: kaboom"),
            "stable panic failure message"
        );
        // Ticks are sequential (run_checks awaits all checks), so >= 3
        // sibling runs imply panicky completed at least its first two runs.
        assert!(
            panicky.contiguous_failures >= 2,
            "panics grow the contiguousFailures streak, got {}",
            panicky.contiguous_failures
        );
        assert!(
            panicky.time_of_first_failure.is_some(),
            "panic streak pins timeOfFirstFailure"
        );
        let sibling = checks.get("sibling").expect("sibling result");
        assert!(sibling.error.is_none(), "sibling unaffected by the panic");

        health.stop().await;
    }

    // ------------------------------------------------------------------
    // update_metrics: a (failing) application check counts against every tag
    // series, and a tag created by a later registration folds in the
    // currently failing application checks (Go worker.updateMetrics +
    // numFailingApplicationChecks).
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn update_metrics_application_fanout_and_new_tag_fold_in() {
        let (health, registry) = new_health();

        // A new application check registers as failing against both the
        // pre-set series: (health, all) and (health, application).
        let (app, _) = counting(Value::Null);
        health
            .register_health_check("app", app, &[APPLICATION_TAG.to_string()])
            .expect("register_health_check(app)");
        assert_eq!(
            gauge_value(&registry, "health", ALL_TAG),
            1.0,
            "(health, all) after registering the application check"
        );
        assert_eq!(
            gauge_value(&registry, "health", APPLICATION_TAG),
            1.0,
            "(health, application) after registering the application check"
        );

        // Registering a check under a brand-new tag folds the currently
        // failing application check into the new tag's series: 1 (the new
        // failing check) + 1 (the failing application check) = 2.
        let (tagged, _) = counting(Value::Null);
        health
            .register_health_check("x", tagged, &["newtag".to_string()])
            .expect("register_health_check(x)");
        assert_eq!(
            gauge_value(&registry, "health", "newtag"),
            2.0,
            "(health, newtag) folds in the failing application check"
        );
        assert_eq!(
            gauge_value(&registry, "health", ALL_TAG),
            2.0,
            "(health, all) counts both failing checks"
        );
        assert_eq!(
            gauge_value(&registry, "health", APPLICATION_TAG),
            1.0,
            "(health, application) unchanged by the non-application check"
        );

        // Both checks passing: the application check decrements every tag
        // series (fan-out), the tagged check its own tag + all.
        health.run_checks_now().await;
        assert_eq!(
            gauge_value(&registry, "health", ALL_TAG),
            0.0,
            "(health, all) after a passing run"
        );
        assert_eq!(
            gauge_value(&registry, "health", APPLICATION_TAG),
            0.0,
            "(health, application) after a passing run"
        );
        assert_eq!(
            gauge_value(&registry, "health", "newtag"),
            0.0,
            "(health, newtag) after a passing run"
        );
    }

    // ------------------------------------------------------------------
    // Monotonic (readiness) caching: once a readiness check passes WITH
    // details the checker never runs again; one passing with no (null)
    // details keeps re-running (Go RegisterMonotonicCheck semantics).
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn monotonic_readiness_caches_only_non_null_details() {
        let (health, _registry) = new_health();
        let (with_details, with_details_runs) = counting(json!({"ready": true}));
        health
            .register_readiness_check("with-details", with_details, &[])
            .expect("register_readiness_check(with-details)");
        let (no_details, no_details_runs) = counting(Value::Null);
        health
            .register_readiness_check("no-details", no_details, &[])
            .expect("register_readiness_check(no-details)");

        health.run_checks_now().await;
        health.run_checks_now().await;
        health.run_checks_now().await;

        assert_eq!(
            with_details_runs.load(Ordering::SeqCst),
            1,
            "a readiness check passing with details is cached and never re-run"
        );
        assert_eq!(
            no_details_runs.load(Ordering::SeqCst),
            3,
            "a readiness check passing with null details keeps re-running"
        );

        // The cached check still reports passing with the cached details.
        let (checks, healthy) = health.readiness(&[]);
        assert!(healthy, "readiness report healthy");
        assert_eq!(
            checks.get("with-details").expect("with-details").details,
            Some(json!({"ready": true})),
            "cached details still reported"
        );
    }

    // ------------------------------------------------------------------
    // Registration error paths: the reserved `all` tag is rejected
    // (errRestrictedTag) and duplicate names are rejected (errDuplicateCheck).
    // ------------------------------------------------------------------
    #[test]
    fn registration_rejects_all_tag_and_duplicates() {
        let (health, _registry) = new_health();
        let (checker, _) = counting(Value::Null);

        assert_matches!(
            health.register_health_check("c", Arc::clone(&checker), &[ALL_TAG.to_string()]),
            Err(HealthError::RestrictedTag(tag)) if tag == ALL_TAG,
            "registering with the reserved `all` tag must fail"
        );

        health
            .register_health_check("c", Arc::clone(&checker), &[])
            .expect("first registration of c");
        assert_matches!(
            health.register_health_check("c", checker, &[]),
            Err(HealthError::DuplicateCheck(name)) if name == "c",
            "duplicate registration must fail"
        );
    }
}
