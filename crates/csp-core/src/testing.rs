//! Test inventory and results, plus the test↔code mapping that distinguishes CSP
//! from simply shipping a coverage report.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::base::Location;
use crate::coverage::RunId;

/// A test known to the server (a single test case, not a file). `id` is opaque
/// and server-assigned; the client uses it for `csp/rangeForTest` lookups.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TestItem {
    pub id: String,
    /// Human-readable name, e.g. `tests::math::adds_two_numbers`.
    pub name: String,
    /// Where the test is defined, if the adapter can locate it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<Location>,
}

/// Outcome of running a single test.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum TestStatus {
    Pass,
    Fail,
    Skip,
}

/// The result of executing one test in a run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TestResult {
    pub id: String,
    pub name: String,
    pub status: TestStatus,
    pub run_id: RunId,
    /// Failure/skip message or assertion output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Location to surface the failure at (e.g. the failing assertion).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<Location>,
    /// Wall-clock duration in milliseconds, if measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<f64>,
}

/// State of a test run, reported via `csp/runStateChanged`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum RunState {
    Running,
    Finished,
    Errored,
}
