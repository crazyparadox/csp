//! # csp-core
//!
//! The single source of truth for the **Coverage Server Protocol (CSP)** — a
//! protocol that does for test quality (coverage, results, quality diagnostics)
//! what LSP does for language intelligence.
//!
//! This crate defines:
//! - the wire **types** ([`base`], [`coverage`], [`testing`], [`capabilities`],
//!   [`messages`]), all deriving [`schemars::JsonSchema`] so the JSON Schema in
//!   `spec/schema/` is generated from exactly these definitions and can never
//!   drift from the implementation;
//! - the [`jsonrpc`] transport (JSON-RPC 2.0 with `Content-Length` framing,
//!   identical to LSP).
//!
//! In CSP v1 the **client is observational** — it cannot drive test runs
//! (`runControl` is `false`). The **server owns collection**: the reference
//! server reuses an existing coverage artifact or runs the project's coverage
//! tool to produce one, then pushes the results. Client-driven run-control
//! methods are declared in [`messages::method`] for forward-compatibility but
//! are unimplemented.

pub mod base;
pub mod capabilities;
pub mod coverage;
pub mod jsonrpc;
pub mod messages;
pub mod testing;

/// The protocol version this crate implements.
pub const CSP_VERSION: &str = "0.1.0";

#[cfg(test)]
mod tests {
    use crate::base::{DiagnosticSeverity, Position, Range};
    use crate::capabilities::{InitializeResult, ServerCapabilities};
    use crate::coverage::{CoverageSummary, FileCoverage, LineCoverage};
    use crate::messages::{PublishCoverageParams, RunStateChangedParams};
    use crate::testing::{RunState, TestResult, TestStatus};

    /// Generic round-trip: any value serializes to JSON and parses back equal.
    fn round_trip<T>(value: &T)
    where
        T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        let json = serde_json::to_string(value).expect("serialize");
        let back: T = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(value, &back, "round-trip mismatch for {json}");
    }

    #[test]
    fn coverage_round_trips() {
        let cov = FileCoverage {
            uri: "file:///src/lib.rs".into(),
            run_id: "run-1".into(),
            version: Some(7),
            stale: false,
            lines: vec![
                LineCoverage { line: 0, hits: 3 },
                LineCoverage { line: 1, hits: 0 },
            ],
            branches: vec![],
            functions: vec![],
            summary: CoverageSummary {
                lines_covered: 1,
                lines_total: 2,
                ..Default::default()
            },
        };
        round_trip(&PublishCoverageParams { coverage: cov });
    }

    #[test]
    fn enums_serialize_as_lowercase_strings() {
        assert_eq!(
            serde_json::to_string(&TestStatus::Pass).unwrap(),
            "\"pass\""
        );
        assert_eq!(serde_json::to_string(&RunState::Running).unwrap(), "\"running\"");
        assert_eq!(
            serde_json::to_string(&DiagnosticSeverity::Warning).unwrap(),
            "\"warning\""
        );
    }

    #[test]
    fn test_result_round_trips() {
        round_trip(&TestResult {
            id: "t1".into(),
            name: "adds_two_numbers".into(),
            status: TestStatus::Fail,
            run_id: "run-1".into(),
            message: Some("assertion failed".into()),
            location: None,
            duration_ms: Some(1.5),
        });
    }

    #[test]
    fn publish_coverage_flattens_file_coverage() {
        // The `#[serde(flatten)]` on PublishCoverageParams means the on-the-wire
        // shape has no nested `coverage` key — `uri` sits at the top level.
        let params = PublishCoverageParams {
            coverage: FileCoverage {
                uri: "file:///a".into(),
                run_id: "r".into(),
                version: None,
                stale: false,
                lines: vec![],
                branches: vec![],
                functions: vec![],
                summary: CoverageSummary::default(),
            },
        };
        let v = serde_json::to_value(&params).unwrap();
        assert_eq!(v["uri"], "file:///a");
        assert!(v.get("coverage").is_none());
    }

    #[test]
    fn run_state_summary_is_optional() {
        let v = serde_json::to_value(RunStateChangedParams {
            run_id: "r".into(),
            state: RunState::Finished,
            message: None,
            summary: None,
        })
        .unwrap();
        assert!(v.get("summary").is_none());
    }

    #[test]
    fn server_capabilities_default_is_report_only() {
        let caps = ServerCapabilities::default();
        assert!(!caps.run_control, "v1 must be report-only by default");
        round_trip(&InitializeResult {
            capabilities: caps,
            server_info: Some("csp-server test".into()),
        });
    }

    #[test]
    fn coverage_summary_ratio() {
        let s = CoverageSummary {
            lines_covered: 3,
            lines_total: 4,
            ..Default::default()
        };
        assert!((s.line_ratio() - 0.75).abs() < f64::EPSILON);
        assert_eq!(CoverageSummary::default().line_ratio(), 1.0);
    }

    #[test]
    fn range_helpers() {
        let r = Range::whole_line(5);
        assert_eq!(r.start, Position::new(5, 0));
        assert_eq!(r.end, Position::new(6, 0));
    }
}
