//! Coverage data model. The granularity ladder is line → branch → function,
//! matching what `cargo-llvm-cov`, `go test -coverprofile` and Istanbul all
//! emit, while staying tool-agnostic on the wire.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::base::{DocumentUri, Range};

/// Identifier for one test run. Coverage and results carry it so the client can
/// correlate everything produced by a single execution and detect supersession.
pub type RunId = String;

/// Per-line execution count.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LineCoverage {
    /// Zero-based line number.
    pub line: u32,
    /// Number of times the line was executed. `0` means uncovered.
    pub hits: u64,
}

/// Branch coverage for a single branch point (e.g. the two arms of an `if`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BranchCoverage {
    /// Source range of the branching construct.
    pub range: Range,
    /// Hit count per branch arm, in source order.
    pub arms: Vec<u64>,
}

/// Function/method coverage.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FunctionCoverage {
    pub name: String,
    /// Range of the function signature/definition.
    pub range: Range,
    /// Times the function was entered. `0` means uncovered.
    pub hits: u64,
}

/// Rolled-up counts for a file or a whole workspace. `covered`/`total` are kept
/// raw so the client can compute percentages and aggregate without precision loss.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CoverageSummary {
    pub lines_covered: u64,
    pub lines_total: u64,
    pub branches_covered: u64,
    pub branches_total: u64,
    pub functions_covered: u64,
    pub functions_total: u64,
}

impl CoverageSummary {
    /// Line coverage as a fraction in `[0.0, 1.0]`; `1.0` when there is nothing
    /// to cover, mirroring how most coverage tools report empty files.
    pub fn line_ratio(&self) -> f64 {
        if self.lines_total == 0 {
            1.0
        } else {
            self.lines_covered as f64 / self.lines_total as f64
        }
    }

    /// Accumulate another summary into this one (for workspace rollups).
    pub fn add(&mut self, other: &CoverageSummary) {
        self.lines_covered += other.lines_covered;
        self.lines_total += other.lines_total;
        self.branches_covered += other.branches_covered;
        self.branches_total += other.branches_total;
        self.functions_covered += other.functions_covered;
        self.functions_total += other.functions_total;
    }
}

/// Coverage for a single file, the unit of the `csp/publishCoverage` push.
///
/// `version` records the document version the coverage was computed against, and
/// `stale` is set by the server once it knows the source has advanced past that
/// version — so the client can grey out or drop markers without re-querying.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FileCoverage {
    pub uri: DocumentUri,
    pub run_id: RunId,
    /// Document version this coverage corresponds to, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<i32>,
    /// Whether the coverage is known to be out of date relative to the open buffer.
    #[serde(default)]
    pub stale: bool,
    pub lines: Vec<LineCoverage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub branches: Vec<BranchCoverage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub functions: Vec<FunctionCoverage>,
    pub summary: CoverageSummary,
}
