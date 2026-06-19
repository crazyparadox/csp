//! Optional per-project configuration, read from `.csp.toml` at the workspace
//! root. This is how a project scopes or overrides coverage collection — the
//! escape hatch for repos where the auto-detected whole-workspace command is too
//! broad or doesn't build (e.g. a large multi-crate monorepo).
//!
//! ```toml
//! # .csp.toml
//! # Run a specific command to produce the coverage artifact (argv, whitespace-split).
//! command = "cargo llvm-cov -p my_crate --ignore-run-fail --lcov --output-path lcov.info"
//! # The artifact that command writes, relative to the root (default: lcov.info).
//! artifact = "lcov.info"
//! # Or force a built-in adapter instead of auto-detection.
//! framework = "go"
//! # Or disable auto-running entirely for this project.
//! autorun = false
//! ```

use std::path::Path;

use serde::Deserialize;

/// Parsed `.csp.toml`. Every field is optional; an absent or malformed file
/// yields the default (auto-detect everything).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ProjectConfig {
    /// Full command (whitespace-split argv) that writes the coverage artifact.
    pub command: Option<String>,
    /// Artifact the command writes, relative to root. Defaults to `lcov.info`.
    pub artifact: Option<String>,
    /// Force a specific built-in adapter by name (e.g. `"go"`, `"istanbul"`).
    pub framework: Option<String>,
    /// Per-project auto-run toggle. `Some(false)` disables it even when the
    /// global `CSP_NO_AUTORUN` is unset.
    pub autorun: Option<bool>,
}

impl ProjectConfig {
    /// Load `<root>/.csp.toml`, or the default if it is absent or unparseable.
    pub fn load(root: &Path) -> ProjectConfig {
        std::fs::read_to_string(root.join(".csp.toml"))
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }
}
