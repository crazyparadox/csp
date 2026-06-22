//! Reference CSP server.
//!
//! On `initialized` it selects a [`CoverageAdapter`] for the workspace and
//! collects coverage — reusing an existing artifact when present, otherwise
//! running the project's coverage tool to generate one (auto-collection; disable
//! with `CSP_NO_AUTORUN`). It then pushes per-file `csp/publishCoverage` (plus
//! `csp/publishQualityDiagnostics` for uncovered lines and a
//! `csp/runStateChanged` envelope). Thereafter it answers pull queries, re-pushes
//! coverage marked `stale` when a document changes (a hint that it may be out of
//! date — the run is not redone), and re-collects on an explicit `csp/run` from
//! the client (force, bypassing the freshness fast-path). The server owns
//! collection; `csp/run` is the only run control in v1 (whole-workspace).
//!
//! The [`Server`] is generic over its output writer so it can be driven with an
//! in-memory buffer in tests; `main.rs` wires it to stdio.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use csp_core::base::{Diagnostic, DiagnosticSeverity, Range};
use csp_core::capabilities::{InitializeParams, InitializeResult};
use csp_core::coverage::{FileCoverage, RunId};
use csp_core::jsonrpc::{self, Id, JsonRpcVersion, Request, Response, ResponseError};
use csp_core::messages::{method, *};
use csp_core::testing::{RunState, TestResult};
use std::collections::VecDeque;
use csp_adapters::{CoverageAdapter, RunData};
use serde::Serialize;
use serde_json::Value;

/// What a run does: a full whole-workspace collection, or a fast selective
/// tests-only run filtered by name.
pub enum RunKind {
    Full { force: bool },
    Selective { filter: Vec<String> },
}

/// A unit of slow work handed to a worker thread: run the adapter off the main
/// message loop so the server keeps answering queries while tests run.
pub struct RunRequest {
    pub run_id: RunId,
    pub kind: RunKind,
    pub root: PathBuf,
    pub adapter: Arc<dyn CoverageAdapter>,
}

/// The result of a [`RunRequest`], fed back to [`Server::apply_run_result`].
pub enum RunOutcome {
    Full(anyhow::Result<RunData>),
    Selective(anyhow::Result<Vec<TestResult>>),
}

impl RunRequest {
    /// Perform the (blocking) work on a worker thread.
    pub fn run(&self) -> RunOutcome {
        match &self.kind {
            RunKind::Full { force } => {
                RunOutcome::Full(self.adapter.collect(&self.root, &self.run_id, *force))
            }
            RunKind::Selective { filter } => {
                RunOutcome::Selective(self.adapter.run_tests(&self.root, filter))
            }
        }
    }
}

/// Outcome of handling one inbound message.
#[derive(PartialEq, Eq)]
pub enum Flow {
    /// Keep serving.
    Continue,
    /// `exit` was received; the loop should stop.
    Exit,
}

pub struct Server<W: Write> {
    out: W,
    root: PathBuf,
    framework_hint: Option<String>,
    adapter: Option<Arc<dyn CoverageAdapter>>,
    /// Latest analysis, keyed by document URI.
    coverage: HashMap<String, FileCoverage>,
    /// Open documents and the version the client last reported.
    open_versions: HashMap<String, i32>,
    run_counter: u64,
    initialized: bool,
    /// Runs ready to spawn now (at most one — the event loop drains and spawns).
    pending_runs: Vec<RunRequest>,
    /// Runs waiting because one is already in flight (serialized; one cargo at a
    /// time). Full-run requests coalesce so the queue can't bloat.
    queue: VecDeque<RunRequest>,
    /// A run is currently being computed on a worker (single-flight).
    in_flight: bool,
    /// Whether the in-flight run is a full run (for full-run coalescing).
    in_flight_is_full: bool,
}

impl<W: Write> Server<W> {
    pub fn new(out: W, root: PathBuf, framework_hint: Option<String>) -> Self {
        Self {
            out,
            root,
            framework_hint,
            adapter: None,
            coverage: HashMap::new(),
            open_versions: HashMap::new(),
            run_counter: 0,
            initialized: false,
            pending_runs: Vec::new(),
            queue: VecDeque::new(),
            in_flight: false,
            in_flight_is_full: false,
        }
    }

    /// Drain the runs queued for worker threads (called by the event loop).
    pub fn take_pending_runs(&mut self) -> Vec<RunRequest> {
        std::mem::take(&mut self.pending_runs)
    }

    /// Dispatch a single request/notification.
    pub fn handle(&mut self, req: Request) -> Flow {
        match req.method.as_str() {
            method::INITIALIZE => self.on_initialize(req),
            method::INITIALIZED => {
                self.initialized = true;
                self.request_run(RunKind::Full { force: false });
                Flow::Continue
            }
            // Explicit "run tests" from the client. No filter → a fresh full run
            // (force, bypassing freshness). A filter → a fast selective tests-only
            // run that updates just those results (coverage untouched).
            method::RUN => {
                let filter = parse_params::<RunParams>(&req)
                    .map(|p| p.filter)
                    .unwrap_or_default();
                if filter.is_empty() {
                    self.request_run(RunKind::Full { force: true });
                } else {
                    self.request_run(RunKind::Selective { filter });
                }
                Flow::Continue
            }
            method::SHUTDOWN => {
                self.respond_ok(req.id, Value::Null);
                Flow::Continue
            }
            method::EXIT => Flow::Exit,

            method::DID_OPEN => self.on_did_open(req),
            method::DID_CHANGE => self.on_did_change(req),
            method::DID_CLOSE => self.on_did_close(req),

            method::COVERAGE => self.on_coverage(req),
            method::SUMMARY => self.on_summary(req),
            method::TESTS_FOR_RANGE => {
                self.respond_ok(req.id, json(&TestsForRangeResult { tests: vec![] }));
                Flow::Continue
            }
            method::RANGE_FOR_TEST => {
                self.respond_ok(req.id, json(&RangeForTestResult { locations: vec![] }));
                Flow::Continue
            }

            // Still reserved / unimplemented.
            method::CANCEL | method::RUN_PROGRESS => {
                self.respond_err(req.id, ResponseError::method_not_found(&req.method));
                Flow::Continue
            }

            other => {
                if req.is_notification() {
                    // Unknown notifications are ignored per the versioning policy.
                    Flow::Continue
                } else {
                    self.respond_err(req.id, ResponseError::method_not_found(other));
                    Flow::Continue
                }
            }
        }
    }

    // --- lifecycle ---

    fn on_initialize(&mut self, req: Request) -> Flow {
        let params: InitializeParams = parse_params(&req).unwrap_or_default();
        if let Some(uri) = params.root_uri.as_deref().and_then(uri_to_path) {
            self.root = uri;
        }
        let hint = params.framework.or_else(|| self.framework_hint.clone());
        self.adapter = csp_adapters::select(&self.root, hint.as_deref()).map(Arc::from);

        let capabilities = self
            .adapter
            .as_ref()
            .map(|a| a.capabilities())
            .unwrap_or_default();
        let server_info = Some(format!(
            "csp-server {} ({})",
            csp_core::CSP_VERSION,
            self.adapter.as_ref().map(|a| a.name()).unwrap_or("none")
        ));
        self.respond_ok(
            req.id,
            json(&InitializeResult {
                capabilities,
                server_info,
            }),
        );
        Flow::Continue
    }

    // --- document sync ---

    fn on_did_open(&mut self, req: Request) -> Flow {
        if let Some(p) = parse_params::<DidOpenParams>(&req) {
            self.open_versions.insert(p.uri.clone(), p.version);
            // If we already analysed this file, stamp the version and re-push.
            if let Some(cov) = self.coverage.get_mut(&p.uri) {
                cov.version = Some(p.version);
                cov.stale = false;
                let cov = cov.clone();
                self.push_coverage(&cov);
            }
        }
        Flow::Continue
    }

    fn on_did_change(&mut self, req: Request) -> Flow {
        if let Some(p) = parse_params::<DidChangeParams>(&req) {
            self.open_versions.insert(p.uri.clone(), p.version);
            // Coverage computed against an older version is now stale.
            if let Some(cov) = self.coverage.get_mut(&p.uri) {
                if cov.version != Some(p.version) {
                    cov.stale = true;
                    let cov = cov.clone();
                    self.push_coverage(&cov);
                    self.push_quality_diagnostics(&cov);
                }
            }
        }
        Flow::Continue
    }

    fn on_did_close(&mut self, req: Request) -> Flow {
        if let Some(p) = parse_params::<DidCloseParams>(&req) {
            self.open_versions.remove(&p.uri);
        }
        Flow::Continue
    }

    // --- queries ---

    fn on_coverage(&mut self, req: Request) -> Flow {
        let coverage = parse_params::<CoverageParams>(&req)
            .and_then(|p| self.coverage.get(&p.uri).cloned());
        self.respond_ok(req.id, json(&CoverageResult { coverage }));
        Flow::Continue
    }

    fn on_summary(&mut self, req: Request) -> Flow {
        let params = parse_params::<SummaryParams>(&req).unwrap_or(SummaryParams { uri: None });
        let summary = match params.uri {
            Some(uri) => self.coverage.get(&uri).map(|c| c.summary).unwrap_or_default(),
            None => {
                let mut s = csp_core::coverage::CoverageSummary::default();
                for c in self.coverage.values() {
                    s.add(&c.summary);
                }
                s
            }
        };
        self.respond_ok(req.id, json(&SummaryResult { summary }));
        Flow::Continue
    }

    // --- analysis + pushes ---

    /// Enqueue a run (non-blocking) and start it if nothing is in flight. Full
    /// runs coalesce — a second full request while one is queued/running is
    /// dropped — so rapid clicks don't pile up redundant whole-workspace runs.
    fn request_run(&mut self, kind: RunKind) {
        if self.adapter.is_none() {
            return;
        }
        if matches!(kind, RunKind::Full { .. }) && self.has_pending_full() {
            return; // coalesce duplicate full runs
        }
        self.queue.push_back(RunRequest {
            run_id: String::new(), // assigned when the run actually starts
            kind,
            root: self.root.clone(),
            adapter: self.adapter.clone().expect("checked above"),
        });
        self.start_next();
    }

    /// True if a full run is already in flight or waiting.
    fn has_pending_full(&self) -> bool {
        (self.in_flight && self.in_flight_is_full)
            || self.queue.iter().any(|r| matches!(r.kind, RunKind::Full { .. }))
    }

    /// If idle, pop the next queued run, assign its id, emit `running` (full runs
    /// only — selective runs don't clear the client's view), and hand it to the
    /// event loop to spawn on a worker.
    fn start_next(&mut self) {
        if self.in_flight || !self.pending_runs.is_empty() {
            return;
        }
        let Some(mut req) = self.queue.pop_front() else {
            return;
        };
        self.run_counter += 1;
        req.run_id = format!("run-{}", self.run_counter);
        self.in_flight = true;
        self.in_flight_is_full = matches!(req.kind, RunKind::Full { .. });
        if self.in_flight_is_full {
            self.notify(
                method::RUN_STATE_CHANGED,
                &RunStateChangedParams {
                    run_id: req.run_id.clone(),
                    state: RunState::Running,
                    message: None,
                    summary: None,
                },
            );
        }
        self.pending_runs.push(req);
    }

    /// Apply a worker's outcome, then start the next queued run.
    pub fn apply_run_result(&mut self, run_id: RunId, outcome: RunOutcome) {
        self.in_flight = false;
        match outcome {
            RunOutcome::Full(result) => self.apply_full(run_id, result),
            RunOutcome::Selective(result) => self.apply_selective(run_id, result),
        }
        self.start_next();
    }

    fn apply_full(&mut self, run_id: RunId, result: anyhow::Result<RunData>) {
        match result {
            Err(e) => {
                let reason = format!("{e:#}");
                eprintln!("[csp-server] analysis failed: {reason}");
                self.notify(
                    method::RUN_STATE_CHANGED,
                    &RunStateChangedParams {
                        run_id,
                        state: RunState::Errored,
                        message: Some(reason),
                        summary: None,
                    },
                );
            }
            Ok(RunData {
                coverage,
                summary,
                results,
                ..
            }) => {
                for mut file in coverage {
                    file.version = self.open_versions.get(&file.uri).copied();
                    self.coverage.insert(file.uri.clone(), file.clone());
                    self.push_coverage(&file);
                    self.push_quality_diagnostics(&file);
                }
                if !results.is_empty() {
                    self.push_test_results(run_id.clone(), results, false);
                }
                self.notify(
                    method::RUN_STATE_CHANGED,
                    &RunStateChangedParams {
                        run_id,
                        state: RunState::Finished,
                        message: None,
                        summary: Some(summary),
                    },
                );
            }
        }
    }

    fn apply_selective(&mut self, run_id: RunId, result: anyhow::Result<Vec<TestResult>>) {
        match result {
            Err(e) => eprintln!("[csp-server] selective run failed: {e:#}"),
            Ok(results) => {
                if !results.is_empty() {
                    // Partial: the client merges these into its existing set; no
                    // coverage and no run-state, so the gutter/summary are intact.
                    self.push_test_results(run_id, results, true);
                }
            }
        }
    }

    fn push_test_results(&mut self, run_id: RunId, mut results: Vec<TestResult>, partial: bool) {
        for r in &mut results {
            r.run_id = run_id.clone();
        }
        self.notify(
            method::PUBLISH_TEST_RESULTS,
            &PublishTestResultsParams {
                run_id,
                results,
                partial,
            },
        );
    }

    /// Test/synchronous driver: run queued requests inline (on this thread) until
    /// none remain. Production uses worker threads via
    /// [`take_pending_runs`](Self::take_pending_runs) + [`apply_run_result`].
    pub fn run_pending_sync(&mut self) {
        while let Some(req) = self.pending_runs.pop() {
            let outcome = req.run();
            self.apply_run_result(req.run_id, outcome);
        }
    }

    fn push_coverage(&mut self, file: &FileCoverage) {
        self.notify(
            method::PUBLISH_COVERAGE,
            &PublishCoverageParams {
                coverage: file.clone(),
            },
        );
    }

    /// Emit a quality diagnostic per uncovered line, so editors that don't render
    /// coverage gutters still surface gaps in their existing problems UI.
    fn push_quality_diagnostics(&mut self, file: &FileCoverage) {
        let diagnostics: Vec<Diagnostic> = file
            .lines
            .iter()
            .filter(|l| l.hits == 0)
            .map(|l| Diagnostic {
                range: Range::whole_line(l.line),
                severity: DiagnosticSeverity::Hint,
                message: "line not covered by tests".to_string(),
                source: Some("csp.coverage".to_string()),
                code: Some("uncovered-line".to_string()),
            })
            .collect();
        self.notify(
            method::PUBLISH_QUALITY_DIAGNOSTICS,
            &PublishQualityDiagnosticsParams {
                uri: file.uri.clone(),
                diagnostics,
            },
        );
    }

    // --- wire helpers ---

    fn respond_ok(&mut self, id: Option<Id>, result: Value) {
        let _ = jsonrpc::write_message(&mut self.out, &Response::ok(id, result));
    }

    fn respond_err(&mut self, id: Option<Id>, error: ResponseError) {
        let _ = jsonrpc::write_message(&mut self.out, &Response::err(id, error));
    }

    fn notify<T: Serialize>(&mut self, method: &str, params: &T) {
        let msg = Request {
            jsonrpc: JsonRpcVersion,
            id: None,
            method: method.to_string(),
            params: Some(serde_json::to_value(params).unwrap_or(Value::Null)),
        };
        let _ = jsonrpc::write_message(&mut self.out, &msg);
    }
}

fn json<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

fn parse_params<T: serde::de::DeserializeOwned>(req: &Request) -> Option<T> {
    req.params
        .clone()
        .and_then(|p| serde_json::from_value(p).ok())
}

/// Map a `file://` URI to a filesystem path, percent-decoding the path so URIs
/// produced by conformant clients (e.g. VS Code) round-trip — a path with a
/// space arrives as `%20` and must be decoded before it names a real file.
/// Accepts the empty-authority (`file:///p`), `localhost`, and no-authority
/// (`file:/p`) forms.
fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri
        .strip_prefix("file://localhost/")
        .map(|r| format!("/{r}"))
        .or_else(|| uri.strip_prefix("file://").map(str::to_string))
        .or_else(|| uri.strip_prefix("file:").map(str::to_string))?;
    Some(PathBuf::from(normalize_uri_path(&percent_decode(&rest))))
}

/// On Windows, turn a URI path (`/C:/dir/f`, forward slashes, leading slash
/// before the drive) into a native path (`C:\dir\f`).
#[cfg(windows)]
fn normalize_uri_path(p: &str) -> String {
    let bytes = p.as_bytes();
    let trimmed = if bytes.len() >= 3
        && bytes[0] == b'/'
        && bytes[1].is_ascii_alphabetic()
        && bytes[2] == b':'
    {
        &p[1..]
    } else {
        p
    };
    trimmed.replace('/', "\\")
}

#[cfg(not(windows))]
fn normalize_uri_path(p: &str) -> String {
    p.to_string()
}

/// Decode `%XX` escapes back into bytes, then interpret as UTF-8. Invalid or
/// truncated escapes are passed through verbatim (best-effort, never panics).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) =
                (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]))
            {
                out.push(hi << 4 | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use csp_core::jsonrpc::{error_code, read_value};
    use std::io::Cursor;

    /// Drain everything the server wrote as untyped JSON values (handles both
    /// responses and notifications, which have different shapes).
    fn drain(buf: &[u8]) -> Vec<Value> {
        let mut cur = Cursor::new(buf);
        let mut msgs = Vec::new();
        while let Ok(v) = read_value(&mut cur) {
            msgs.push(v);
        }
        msgs
    }

    fn req(id: i64, method: &str, params: Value) -> Request {
        Request {
            jsonrpc: JsonRpcVersion,
            id: Some(Id::Number(id)),
            method: method.to_string(),
            params: Some(params),
        }
    }

    fn notif(method: &str, params: Value) -> Request {
        Request {
            jsonrpc: JsonRpcVersion,
            id: None,
            method: method.to_string(),
            params: Some(params),
        }
    }

    /// Build a unique temp workspace with an LCOV artifact, returning its path.
    fn temp_lcov_workspace(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("csp-test-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        std::fs::write(
            dir.join("lcov.info"),
            "SF:src/lib.rs\nDA:1,3\nDA:2,0\nLF:2\nLH:1\nend_of_record\n",
        )
        .unwrap();
        dir
    }

    #[test]
    fn initialize_advertises_adapter_capabilities() {
        let root = temp_lcov_workspace("init");
        let mut buf = Vec::new();
        {
            let mut server = Server::new(&mut buf, root, None);
            server.handle(req(1, method::INITIALIZE, Value::Null));
        }
        let msgs = drain(&buf);
        let caps = &msgs[0]["result"]["capabilities"];
        assert_eq!(caps["coverage"]["line"], true);
        assert_eq!(caps["runControl"], true); // server implements csp/run
        assert_eq!(msgs[0]["id"], 1);
    }

    #[test]
    fn initialized_pushes_coverage_and_diagnostics() {
        let root = temp_lcov_workspace("pushes");
        let mut buf = Vec::new();
        {
            let mut server = Server::new(&mut buf, root, None);
            server.handle(req(1, method::INITIALIZE, Value::Null));
            server.handle(notif(method::INITIALIZED, Value::Null));
            server.run_pending_sync();
        }
        let msgs = drain(&buf);
        let methods: Vec<&str> = msgs.iter().filter_map(|m| m["method"].as_str()).collect();

        assert!(methods.contains(&method::PUBLISH_COVERAGE));
        assert!(methods.contains(&method::PUBLISH_QUALITY_DIAGNOSTICS));
        assert!(methods.contains(&method::RUN_STATE_CHANGED));

        // The publishCoverage payload carries one covered + one uncovered line.
        let cov = msgs
            .iter()
            .find(|m| m["method"] == method::PUBLISH_COVERAGE)
            .unwrap();
        assert_eq!(cov["params"]["summary"]["linesCovered"], 1);
        assert_eq!(cov["params"]["summary"]["linesTotal"], 2);
        assert!(cov["params"]["runId"].is_string());

        // One uncovered line => one quality diagnostic.
        let diag = msgs
            .iter()
            .find(|m| m["method"] == method::PUBLISH_QUALITY_DIAGNOSTICS)
            .unwrap();
        assert_eq!(diag["params"]["diagnostics"].as_array().unwrap().len(), 1);
        assert_eq!(diag["params"]["diagnostics"][0]["code"], "uncovered-line");
    }

    #[test]
    fn did_change_marks_coverage_stale() {
        let root = temp_lcov_workspace("stale");
        let mut buf = Vec::new();
        {
            let mut server = Server::new(&mut buf, root, None);
            server.handle(req(1, method::INITIALIZE, Value::Null));
            server.handle(notif(method::INITIALIZED, Value::Null));
            server.run_pending_sync();

            let uri = server.coverage.keys().next().cloned().unwrap();
            server.handle(notif(
                method::DID_OPEN,
                serde_json::json!({ "uri": uri, "version": 1 }),
            ));
            server.handle(notif(
                method::DID_CHANGE,
                serde_json::json!({ "uri": uri, "version": 2 }),
            ));
        }
        // The last publishCoverage (post-didChange) must be flagged stale; the
        // initial push from `initialized` was not.
        let msgs = drain(&buf);
        let stale_push = msgs
            .iter()
            .filter(|m| m["method"] == method::PUBLISH_COVERAGE)
            .last()
            .expect("a coverage re-push after didChange");
        assert_eq!(stale_push["params"]["stale"], true);
    }

    #[test]
    fn cancel_is_method_not_found() {
        let root = temp_lcov_workspace("runctl");
        let mut buf = Vec::new();
        {
            let mut server = Server::new(&mut buf, root, None);
            server.handle(req(5, method::CANCEL, Value::Null));
        }
        let msgs = drain(&buf);
        assert_eq!(msgs[0]["error"]["code"], error_code::METHOD_NOT_FOUND);
    }

    #[test]
    fn run_reanalyzes_and_pushes_coverage() {
        let root = temp_lcov_workspace("rerun");
        let mut buf = Vec::new();
        {
            let mut server = Server::new(&mut buf, root, None);
            server.handle(req(1, method::INITIALIZE, Value::Null));
            server.handle(notif(method::INITIALIZED, Value::Null));
            server.run_pending_sync(); // initial run completes
            server.handle(req(2, method::RUN, Value::Null));
            server.run_pending_sync(); // explicit re-run completes
        }
        // The explicit run produces a second batch of coverage + run-state pushes.
        let msgs = drain(&buf);
        let coverage_pushes = msgs
            .iter()
            .filter(|m| m["method"] == method::PUBLISH_COVERAGE)
            .count();
        assert!(coverage_pushes >= 2, "initial run + forced re-run");
    }

    #[test]
    fn run_with_filter_queues_a_selective_run() {
        let root = temp_lcov_workspace("selective");
        let mut buf = Vec::new();
        let mut server = Server::new(&mut buf, root, None);
        server.handle(req(1, method::INITIALIZE, Value::Null));
        server.handle(notif(method::INITIALIZED, Value::Null));
        server.run_pending_sync(); // full run done; server idle

        server.handle(req(2, method::RUN, serde_json::json!({ "filter": ["adds"] })));
        // A filtered run is selective (would `cargo test adds`), not a full run.
        assert_eq!(server.pending_runs.len(), 1);
        assert!(matches!(
            server.pending_runs[0].kind,
            RunKind::Selective { .. }
        ));
    }

    #[test]
    fn queries_answered_while_run_in_flight() {
        // After `initialized` the run is queued (in flight) but NOT yet executed
        // — handling a query must still respond immediately, never block on it.
        let root = temp_lcov_workspace("nonblock");
        let mut buf = Vec::new();
        {
            let mut server = Server::new(&mut buf, root, None);
            server.handle(req(1, method::INITIALIZE, Value::Null));
            server.handle(notif(method::INITIALIZED, Value::Null));
            // A run is now in flight (not run_pending_sync'd). Query anyway:
            assert!(server.in_flight, "run should be in flight");
            server.handle(req(2, method::SUMMARY, Value::Null));
            // Coalescing: a second full run while one is in flight is dropped.
            server.handle(req(3, method::RUN, Value::Null));
            assert_eq!(server.pending_runs.len(), 1, "single-flight: one run");
            assert!(server.queue.is_empty(), "duplicate full run coalesced away");
        }
        let msgs = drain(&buf);
        assert!(
            msgs.iter().any(|m| m["id"] == 2 && m["result"].is_object()),
            "summary query answered while the run was in flight",
        );
    }

    #[test]
    fn coverage_query_returns_cached_data() {
        let root = temp_lcov_workspace("query");
        let mut buf = Vec::new();
        {
            let mut server = Server::new(&mut buf, root, None);
            server.handle(req(1, method::INITIALIZE, Value::Null));
            server.handle(notif(method::INITIALIZED, Value::Null));
            server.run_pending_sync();
            let uri = server.coverage.keys().next().cloned().unwrap();
            server.handle(req(2, method::COVERAGE, serde_json::json!({ "uri": uri })));
        }
        let msgs = drain(&buf);
        let resp = msgs.iter().find(|m| m["id"] == 2).expect("coverage response");
        assert_eq!(resp["result"]["coverage"]["summary"]["linesTotal"], 2);
    }

    #[test]
    fn uri_to_path_percent_decodes() {
        assert_eq!(
            uri_to_path("file:///tmp/csp%20space/go.mod"),
            Some(PathBuf::from("/tmp/csp space/go.mod"))
        );
        // Non-ASCII: 'é' = %C3%A9 in UTF-8.
        assert_eq!(
            uri_to_path("file:///caf%C3%A9/x.rs"),
            Some(PathBuf::from("/café/x.rs"))
        );
        // Plain paths are unchanged.
        assert_eq!(
            uri_to_path("file:///a/b/c.rs"),
            Some(PathBuf::from("/a/b/c.rs"))
        );
    }

    #[cfg(windows)]
    #[test]
    fn uri_to_path_windows_drive_letter() {
        assert_eq!(
            uri_to_path("file:///C:/proj/a%20b.rs"),
            Some(PathBuf::from("C:\\proj\\a b.rs"))
        );
    }

    #[test]
    fn exit_signals_stop() {
        let mut buf = Vec::new();
        let mut server = Server::new(&mut buf, PathBuf::from("/tmp"), None);
        assert!(server.handle(notif(method::EXIT, Value::Null)) == Flow::Exit);
    }
}
