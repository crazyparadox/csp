//! Method names and the parameter/result payloads for every CSP method.
//!
//! Everything lives under the `csp/` namespace so CSP traffic coexists with LSP
//! `textDocument/` traffic on a shared or parallel channel. The [`method`] module
//! holds the canonical method-name string constants used by both client and
//! server to avoid typos drifting the two apart.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::base::{DocumentUri, Location, Range};
use crate::coverage::{CoverageSummary, FileCoverage, RunId};
use crate::testing::{RunState, TestItem, TestResult};

/// Canonical method-name constants.
pub mod method {
    // Lifecycle
    pub const INITIALIZE: &str = "initialize";
    pub const INITIALIZED: &str = "initialized";
    pub const SHUTDOWN: &str = "shutdown";
    pub const EXIT: &str = "exit";

    // Document sync (client → server notifications)
    pub const DID_OPEN: &str = "csp/didOpen";
    pub const DID_CHANGE: &str = "csp/didChange";
    pub const DID_CLOSE: &str = "csp/didClose";

    // Push notifications (server → client)
    pub const PUBLISH_COVERAGE: &str = "csp/publishCoverage";
    pub const PUBLISH_TEST_RESULTS: &str = "csp/publishTestResults";
    pub const PUBLISH_QUALITY_DIAGNOSTICS: &str = "csp/publishQualityDiagnostics";
    pub const RUN_STATE_CHANGED: &str = "csp/runStateChanged";

    // Pull requests (client → server)
    pub const COVERAGE: &str = "csp/coverage";
    pub const TESTS_FOR_RANGE: &str = "csp/testsForRange";
    pub const RANGE_FOR_TEST: &str = "csp/rangeForTest";
    pub const SUMMARY: &str = "csp/summary";

    // Reserved for v-next (run control). Declared for forward-compatibility; the
    // report-only server rejects these with a MethodNotFound error.
    pub const RUN: &str = "csp/run";
    pub const CANCEL: &str = "csp/cancel";
    pub const RUN_PROGRESS: &str = "csp/runProgress";
}

// --- document sync params ---

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DidOpenParams {
    pub uri: DocumentUri,
    pub version: i32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DidChangeParams {
    pub uri: DocumentUri,
    pub version: i32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DidCloseParams {
    pub uri: DocumentUri,
}

// --- push notification params (server → client) ---

/// Params of `csp/publishCoverage`. One notification per file keeps payloads
/// small and lets the client update incrementally as a run completes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PublishCoverageParams {
    #[serde(flatten)]
    pub coverage: FileCoverage,
}

/// Params of `csp/publishTestResults`. Results are batched per notification but
/// always tagged with `run_id` so the client can group them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PublishTestResultsParams {
    pub run_id: RunId,
    pub results: Vec<TestResult>,
}

/// Params of `csp/publishQualityDiagnostics`. Mirrors LSP `publishDiagnostics`:
/// a full replacement set for the given file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PublishQualityDiagnosticsParams {
    pub uri: DocumentUri,
    pub diagnostics: Vec<crate::base::Diagnostic>,
}

/// Params of `csp/runStateChanged`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunStateChangedParams {
    pub run_id: RunId,
    pub state: RunState,
    /// Human-readable detail, primarily the reason on `errored` (e.g. the tool
    /// failed or isn't installed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<CoverageSummary>,
}

// --- pull request params/results (client → server) ---

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CoverageParams {
    pub uri: DocumentUri,
    /// Optional sub-range; omitted means the whole file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<Range>,
}

/// Result of `csp/coverage`. `null`/absent when no coverage is known for the file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CoverageResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage: Option<FileCoverage>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TestsForRangeParams {
    pub uri: DocumentUri,
    pub range: Range,
}

/// Result of `csp/testsForRange` — the tests that exercise the given range.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TestsForRangeResult {
    pub tests: Vec<TestItem>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RangeForTestParams {
    pub test_id: String,
}

/// Result of `csp/rangeForTest` — the source locations a test covers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RangeForTestResult {
    pub locations: Vec<Location>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SummaryParams {
    /// Omitted means the whole workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<DocumentUri>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SummaryResult {
    pub summary: CoverageSummary,
}
