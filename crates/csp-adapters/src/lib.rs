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

pub mod config;
pub mod go;
pub mod istanbul;
pub mod lcov;

mod adapters;
pub use adapters::{ConfiguredAdapter, GenericLcovAdapter, GoAdapter, RustAdapter, TsAdapter};
pub use config::ProjectConfig;

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
    /// `run_id`. Reuses a fresh existing artifact; otherwise (or when `force` is
    /// set — an explicit "run tests" request) generates one via
    /// [`generate`](Self::generate) before parsing. Returns an error if no
    /// artifact can be obtained or it is malformed.
    fn collect(&self, root: &Path, run_id: &RunId, force: bool) -> anyhow::Result<RunData>;
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

/// Pick an adapter for `root`, honoring `<root>/.csp.toml` first:
/// - a `command` yields a [`ConfiguredAdapter`] (scoped/overridden collection);
/// - otherwise `hint` (or the config's `framework`) selects a built-in by name;
/// - otherwise the first built-in whose `detect` returns true wins.
pub fn select(root: &Path, hint: Option<&str>) -> Option<Box<dyn CoverageAdapter>> {
    let cfg = ProjectConfig::load(root);
    let autorun = cfg.autorun.unwrap_or(true);

    if let Some(command) = &cfg.command {
        let argv: Vec<String> = command.split_whitespace().map(str::to_owned).collect();
        if !argv.is_empty() {
            let artifact = cfg.artifact.clone().unwrap_or_else(|| "lcov.info".to_string());
            return Some(Box::new(ConfiguredAdapter::new(argv, artifact, autorun)));
        }
    }

    let framework = hint.map(str::to_owned).or(cfg.framework);
    if let Some(name) = &framework {
        if let Some(a) = registry().into_iter().find(|a| a.name() == name) {
            return Some(a);
        }
    }
    registry().into_iter().find(|a| a.detect(root))
}

/// Convert a filesystem path to an RFC 3986 `file://` URI, making it absolute
/// against `root` when relative. Best-effort: falls back to the lexical join when
/// the path does not exist on disk (e.g. coverage for a deleted file).
///
/// The path is percent-encoded so the result is a conformant URI that a strict
/// client (e.g. VS Code's `Uri.parse`) decodes back to the original path. Paths
/// containing spaces or other reserved characters would otherwise produce
/// malformed URIs that no client could match against an open document.
pub fn path_to_uri(root: &Path, path: &str) -> String {
    let p = Path::new(path);
    let abs: PathBuf = if p.is_absolute() {
        p.to_path_buf()
    } else {
        root.join(p)
    };
    // Normalise `.`/`..` lexically rather than calling `canonicalize`: resolving
    // symlinks would yield a real path (e.g. `/tmp` → `/private/tmp` on macOS)
    // that no longer matches the URI the editor sends for the same open file.
    let abs = lexically_normalize(&abs);
    format!("file://{}", percent_encode_path(&to_uri_path(&abs)))
}

/// Resolve `.` and `..` components without touching the filesystem.
fn lexically_normalize(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Render an absolute path as the path portion of a `file://` URI. On Windows
/// this maps `C:\dir\f` to `/C:/dir/f` (forward slashes, leading slash before
/// the drive) so the result is a conformant `file:///C:/dir/f`.
#[cfg(windows)]
fn to_uri_path(abs: &Path) -> String {
    let forward = abs.to_string_lossy().replace('\\', "/");
    if forward.starts_with('/') {
        forward
    } else {
        format!("/{forward}")
    }
}

#[cfg(not(windows))]
fn to_uri_path(abs: &Path) -> String {
    abs.to_string_lossy().into_owned()
}

/// Percent-encode a filesystem path for use in a `file://` URI. Path separators
/// (`/`) and the RFC 3986 unreserved set (`A-Z a-z 0-9 - . _ ~`) are preserved;
/// every other byte is encoded as `%XX`. Encoding the UTF-8 bytes keeps non-ASCII
/// paths valid.
fn percent_encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for &byte in path.as_bytes() {
        match byte {
            // Unreserved set, plus `/` (separator) and `:` (Windows drive colon;
            // valid in a URI path and kept unencoded by VS Code).
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                out.push(byte as char);
            }
            _ => {
                out.push('%');
                out.push(hex_digit(byte >> 4));
                out.push(hex_digit(byte & 0x0f));
            }
        }
    }
    out
}

fn hex_digit(nibble: u8) -> char {
    match nibble {
        0..=9 => (b'0' + nibble) as char,
        _ => (b'A' + (nibble - 10)) as char,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_encode_preserves_separators_and_unreserved() {
        assert_eq!(
            percent_encode_path("/Users/me/proj/src/math.rs"),
            "/Users/me/proj/src/math.rs"
        );
    }

    #[test]
    fn percent_encode_escapes_spaces_and_reserved() {
        assert_eq!(
            percent_encode_path("/a b/c#d?/e.rs"),
            "/a%20b/c%23d%3F/e.rs"
        );
    }

    #[test]
    fn percent_encode_escapes_non_ascii_as_utf8_bytes() {
        // "café" → 'é' is U+00E9 = 0xC3 0xA9 in UTF-8.
        assert_eq!(percent_encode_path("/caf\u{e9}"), "/caf%C3%A9");
    }

    #[test]
    fn lexically_normalize_resolves_dot_segments() {
        assert_eq!(
            lexically_normalize(Path::new("/a/b/../c/./d")),
            PathBuf::from("/a/c/d")
        );
    }

    #[cfg(windows)]
    #[test]
    fn path_to_uri_windows_drive_letter() {
        let uri = path_to_uri(Path::new("C:\\proj"), "src\\a b.rs");
        assert_eq!(uri, "file:///C:/proj/src/a%20b.rs");
    }

    #[test]
    fn path_to_uri_encodes_relative_path_under_root() {
        // Use a non-existent root so canonicalize is a no-op and the result is
        // the deterministic lexical join.
        let root = Path::new("/no/such/root dir");
        let uri = path_to_uri(root, "a b.rs");
        assert_eq!(uri, "file:///no/such/root%20dir/a%20b.rs");
    }
}
