//! Reference CSP server.
//!
//! On `initialized` it selects a [`CoverageAdapter`] for the workspace and
//! collects coverage — reusing an existing artifact when present, otherwise
//! running the project's coverage tool to generate one (auto-collection; disable
//! with `CSP_NO_AUTORUN`). It then pushes per-file `csp/publishCoverage` (plus
//! `csp/publishQualityDiagnostics` for uncovered lines and a
//! `csp/runStateChanged` envelope). Thereafter it answers pull queries and
//! re-pushes coverage marked `stale` when a document changes. The client cannot
//! drive runs (`runControl` is `false`); the server owns collection.
//!
//! The [`Server`] is generic over its output writer so it can be driven with an
//! in-memory buffer in tests; `main.rs` wires it to stdio.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;

use csp_core::base::{Diagnostic, DiagnosticSeverity, Range};
use csp_core::capabilities::{InitializeParams, InitializeResult};
use csp_core::coverage::{FileCoverage, RunId};
use csp_core::jsonrpc::{self, Id, JsonRpcVersion, Request, Response, ResponseError};
use csp_core::messages::{method, *};
use csp_core::testing::RunState;
use csp_adapters::{CoverageAdapter, RunData};
use serde::Serialize;
use serde_json::Value;

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
    adapter: Option<Box<dyn CoverageAdapter>>,
    /// Latest analysis, keyed by document URI.
    coverage: HashMap<String, FileCoverage>,
    /// Open documents and the version the client last reported.
    open_versions: HashMap<String, i32>,
    run_counter: u64,
    initialized: bool,
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
        }
    }

    /// Dispatch a single request/notification.
    pub fn handle(&mut self, req: Request) -> Flow {
        match req.method.as_str() {
            method::INITIALIZE => self.on_initialize(req),
            method::INITIALIZED => {
                self.initialized = true;
                self.run_analysis();
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

            // Reserved run-control methods are unimplemented in report-only v1.
            method::RUN | method::CANCEL | method::RUN_PROGRESS => {
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
        self.adapter = csp_adapters::select(&self.root, hint.as_deref());

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

    /// Run the selected adapter and push everything it produced.
    fn run_analysis(&mut self) {
        // Take the adapter out so we can call `&mut self` push helpers while using
        // it; it is restored before returning.
        let Some(adapter) = self.adapter.take() else {
            return;
        };
        self.run_counter += 1;
        let run_id: RunId = format!("run-{}", self.run_counter);

        self.notify(
            method::RUN_STATE_CHANGED,
            &RunStateChangedParams {
                run_id: run_id.clone(),
                state: RunState::Running,
                message: None,
                summary: None,
            },
        );

        let data = match adapter.collect(&self.root, &run_id) {
            Ok(d) => d,
            Err(e) => {
                let reason = format!("{e:#}");
                self.notify(
                    method::RUN_STATE_CHANGED,
                    &RunStateChangedParams {
                        run_id,
                        state: RunState::Errored,
                        message: Some(reason.clone()),
                        summary: None,
                    },
                );
                eprintln!("[csp-server] analysis failed: {reason}");
                self.adapter = Some(adapter);
                return;
            }
        };

        let RunData {
            coverage,
            summary,
            results,
            ..
        } = data;
        for mut file in coverage {
            // Stamp the version if the file is open, so staleness works later.
            file.version = self.open_versions.get(&file.uri).copied();
            self.coverage.insert(file.uri.clone(), file.clone());
            self.push_coverage(&file);
            self.push_quality_diagnostics(&file);
        }

        // Push test pass/fail/skip outcomes, if the adapter produced any.
        if !results.is_empty() {
            self.notify(
                method::PUBLISH_TEST_RESULTS,
                &PublishTestResultsParams {
                    run_id: run_id.clone(),
                    results,
                },
            );
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
        self.adapter = Some(adapter);
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

/// Map a `file://` URI to a filesystem path.
fn uri_to_path(uri: &str) -> Option<PathBuf> {
    uri.strip_prefix("file://").map(PathBuf::from)
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
        assert_eq!(caps["runControl"], false);
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
    fn run_control_is_method_not_found() {
        let root = temp_lcov_workspace("runctl");
        let mut buf = Vec::new();
        {
            let mut server = Server::new(&mut buf, root, None);
            server.handle(req(5, method::RUN, Value::Null));
        }
        let msgs = drain(&buf);
        assert_eq!(msgs[0]["error"]["code"], error_code::METHOD_NOT_FOUND);
    }

    #[test]
    fn coverage_query_returns_cached_data() {
        let root = temp_lcov_workspace("query");
        let mut buf = Vec::new();
        {
            let mut server = Server::new(&mut buf, root, None);
            server.handle(req(1, method::INITIALIZE, Value::Null));
            server.handle(notif(method::INITIALIZED, Value::Null));
            let uri = server.coverage.keys().next().cloned().unwrap();
            server.handle(req(2, method::COVERAGE, serde_json::json!({ "uri": uri })));
        }
        let msgs = drain(&buf);
        let resp = msgs.iter().find(|m| m["id"] == 2).expect("coverage response");
        assert_eq!(resp["result"]["coverage"]["summary"]["linesTotal"], 2);
    }

    #[test]
    fn exit_signals_stop() {
        let mut buf = Vec::new();
        let mut server = Server::new(&mut buf, PathBuf::from("/tmp"), None);
        assert!(server.handle(notif(method::EXIT, Value::Null)) == Flow::Exit);
    }
}
