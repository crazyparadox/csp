//! Lifecycle and capability-negotiation types. As in LSP, the client and server
//! exchange capabilities during `initialize` so each side can degrade gracefully:
//! a client that can't render branch coverage simply ignores it, and a client
//! learns up-front that this server is report-only (`run_control = false`).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Which coverage granularities a server can produce.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CoverageCapabilities {
    pub line: bool,
    pub branch: bool,
    pub function: bool,
}

/// Quality domains a server supports. Each flag gates a family of methods, so a
/// client can hide UI for domains the server doesn't implement.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ServerCapabilities {
    /// Emits `csp/publishCoverage` and answers `csp/coverage`.
    pub coverage: CoverageCapabilities,
    /// Emits `csp/publishTestResults` and `csp/runStateChanged`.
    pub test_results: bool,
    /// Answers `csp/testsForRange` and `csp/rangeForTest`.
    pub test_mapping: bool,
    /// Emits `csp/publishQualityDiagnostics`.
    pub quality_diagnostics: bool,
    /// Reserved for v-next. Always `false` in the report-only v1: the server
    /// observes existing test output rather than executing tests itself.
    pub run_control: bool,
}

/// Capabilities the client advertises to the server.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClientCapabilities {
    /// Client can render coverage in the editor gutter.
    pub coverage_gutter: bool,
    /// Client can display test results inline / in a panel.
    pub test_results: bool,
    /// Client understands `stale` flags and will visually distinguish stale data.
    pub staleness: bool,
}

/// Parameters of the `initialize` request.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    /// Absolute path of the workspace root the server should analyse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_uri: Option<String>,
    pub capabilities: ClientCapabilities,
    /// Optional hint naming the test framework/tool to use (e.g. `"cargo-llvm-cov"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub framework: Option<String>,
}

/// Result of the `initialize` request.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub capabilities: ServerCapabilities,
    /// Server name/version for logging, e.g. `"csp-server 0.1.0 (rust/llvm-cov)"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_info: Option<String>,
}
