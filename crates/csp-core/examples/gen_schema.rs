//! Generates JSON Schema files for every CSP message into `spec/schema/`.
//!
//! Run from the workspace root with:
//!
//! ```sh
//! cargo run -p csp-core --example gen_schema
//! ```
//!
//! The schemas are derived directly from the `csp-core` types, so the spec's
//! machine-checkable contract can never drift from the implementation.

use std::fs;
use std::path::PathBuf;

use schemars::{schema_for, JsonSchema};

/// Write the schema for `T` to `spec/schema/<name>.json`.
fn emit<T: JsonSchema>(dir: &PathBuf, name: &str) {
    let schema = schema_for!(T);
    let json = serde_json::to_string_pretty(&schema).expect("serialize schema");
    let path = dir.join(format!("{name}.json"));
    fs::write(&path, json + "\n").expect("write schema file");
    println!("wrote {}", path.display());
}

fn main() {
    use csp_core::base::Diagnostic;
    use csp_core::capabilities::{InitializeParams, InitializeResult};
    use csp_core::coverage::FileCoverage;
    use csp_core::messages::*;
    use csp_core::testing::{TestItem, TestResult};

    // Resolve <workspace>/spec/schema relative to this crate's manifest.
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("spec")
        .join("schema");
    fs::create_dir_all(&dir).expect("create spec/schema dir");

    // Lifecycle
    emit::<InitializeParams>(&dir, "initialize_params");
    emit::<InitializeResult>(&dir, "initialize_result");

    // Document sync
    emit::<DidOpenParams>(&dir, "did_open_params");
    emit::<DidChangeParams>(&dir, "did_change_params");
    emit::<DidCloseParams>(&dir, "did_close_params");

    // Push notifications
    emit::<PublishCoverageParams>(&dir, "publish_coverage_params");
    emit::<PublishTestResultsParams>(&dir, "publish_test_results_params");
    emit::<PublishQualityDiagnosticsParams>(&dir, "publish_quality_diagnostics_params");
    emit::<RunStateChangedParams>(&dir, "run_state_changed_params");

    // Pull requests
    emit::<CoverageParams>(&dir, "coverage_params");
    emit::<CoverageResult>(&dir, "coverage_result");
    emit::<TestsForRangeParams>(&dir, "tests_for_range_params");
    emit::<TestsForRangeResult>(&dir, "tests_for_range_result");
    emit::<RangeForTestParams>(&dir, "range_for_test_params");
    emit::<RangeForTestResult>(&dir, "range_for_test_result");
    emit::<SummaryParams>(&dir, "summary_params");
    emit::<SummaryResult>(&dir, "summary_result");

    // Shared component types (useful as standalone references in the spec)
    emit::<FileCoverage>(&dir, "file_coverage");
    emit::<TestResult>(&dir, "test_result");
    emit::<TestItem>(&dir, "test_item");
    emit::<Diagnostic>(&dir, "diagnostic");

    println!("\nGenerated schemas in {}", dir.display());
}
