//! # csp-adapters
//!
//! Translates the coverage/test output of real tools into CSP types. Each
//! [`CoverageAdapter`] knows how to (a) recognise a workspace it applies to and
//! (b) parse that workspace's coverage artifact into [`RunData`].
//!
//! CSP v1 is report-only, so adapters **read existing artifacts** (an `lcov.info`,
//! a `coverage.out`, an Istanbul `coverage-final.json`) rather than running tests.
//! The protocol never mentions these formats; that translation is exactly the job
//! this crate factors out.

use std::path::{Path, PathBuf};

use csp_core::coverage::{CoverageSummary, FileCoverage, RunId};
use csp_core::testing::{TestItem, TestResult};

pub mod go;
pub mod istanbul;
pub mod lcov;

mod adapters;
pub use adapters::{GenericLcovAdapter, GoAdapter, RustAdapter, TsAdapter};

/// Everything one analysis pass produces. In v1 `tests`/`results` may be empty
/// when the artifact is coverage-only; `coverage` is always the primary payload.
#[derive(Clone, Debug, Default)]
pub struct RunData {
    pub run_id: RunId,
    pub coverage: Vec<FileCoverage>,
    pub tests: Vec<TestItem>,
    pub results: Vec<TestResult>,
    pub summary: CoverageSummary,
}

impl RunData {
    /// Build a [`RunData`] from per-file coverage, computing the workspace rollup.
    pub fn from_coverage(run_id: RunId, coverage: Vec<FileCoverage>) -> Self {
        let mut summary = CoverageSummary::default();
        for f in &coverage {
            summary.add(&f.summary);
        }
        Self {
            run_id,
            coverage,
            tests: Vec::new(),
            results: Vec::new(),
            summary,
        }
    }
}

/// An adapter that turns one ecosystem's coverage output into CSP data.
pub trait CoverageAdapter: Send + Sync {
    /// Stable identifier, also accepted as the `framework` hint in `initialize`
    /// (e.g. `"cargo-llvm-cov"`, `"go"`, `"istanbul"`, `"lcov"`).
    fn name(&self) -> &'static str;

    /// Whether this adapter applies to the given workspace root. Cheap checks
    /// only (manifest presence, artifact existence).
    fn detect(&self, root: &Path) -> bool;

    /// Capabilities this adapter can satisfy, used to build the server's
    /// `initialize` response.
    fn capabilities(&self) -> csp_core::capabilities::ServerCapabilities;

    /// Run the project's coverage tool to (re)generate its artifact. Called by
    /// [`collect`](Self::collect) when no artifact exists yet (unless
    /// auto-running is disabled). The default is a no-op, for adapters that only
    /// consume an artifact someone else produced.
    fn generate(&self, _root: &Path) -> anyhow::Result<()> {
        Ok(())
    }

    /// Produce [`RunData`] for the workspace, stamping each payload with
    /// `run_id`. Reuses an existing artifact when present; otherwise generates
    /// one via [`generate`](Self::generate) before parsing. Returns an error if
    /// no artifact can be obtained or it is malformed.
    fn collect(&self, root: &Path, run_id: &RunId) -> anyhow::Result<RunData>;
}

/// The built-in adapters, in detection-priority order (most specific first, the
/// generic lcov fallback last).
pub fn registry() -> Vec<Box<dyn CoverageAdapter>> {
    vec![
        Box::new(RustAdapter),
        Box::new(GoAdapter),
        Box::new(TsAdapter),
        Box::new(GenericLcovAdapter),
    ]
}

/// Pick an adapter for `root`. If `hint` names a known adapter it wins; otherwise
/// the first adapter whose `detect` returns true is used.
pub fn select(root: &Path, hint: Option<&str>) -> Option<Box<dyn CoverageAdapter>> {
    let reg = registry();
    if let Some(hint) = hint {
        if let Some(a) = registry().into_iter().find(|a| a.name() == hint) {
            return Some(a);
        }
    }
    reg.into_iter().find(|a| a.detect(root))
}

/// Convert a filesystem path to a `file://` URI, making it absolute against
/// `root` when relative. Best-effort: falls back to the lexical join when the
/// path does not exist on disk (e.g. coverage for a deleted file).
pub fn path_to_uri(root: &Path, path: &str) -> String {
    let p = Path::new(path);
    let abs: PathBuf = if p.is_absolute() {
        p.to_path_buf()
    } else {
        root.join(p)
    };
    let abs = abs.canonicalize().unwrap_or(abs);
    format!("file://{}", abs.to_string_lossy())
}
