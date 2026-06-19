//! Concrete adapters: detect a workspace by its project markers, ensure a
//! coverage artifact exists — **generating one by running the project's coverage
//! tool when it is missing** — and parse it with the appropriate format parser.
//!
//! Detection keys on project markers (`Cargo.toml`, `go.mod`, `package.json`), not
//! on a pre-existing artifact, so coverage works the moment a project is opened
//! without the user having to run anything by hand. If an artifact is already
//! present it is reused (fast path); otherwise [`CoverageAdapter::generate`] runs
//! the tool (`cargo llvm-cov`, `go test -coverprofile`, `vitest --coverage`).
//! Auto-generation can be disabled by setting `CSP_NO_AUTORUN` (then a missing
//! artifact is simply reported as no coverage).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context;
use csp_core::base::{Location, Range};
use csp_core::capabilities::{CoverageCapabilities, ServerCapabilities};
use csp_core::coverage::RunId;
use csp_core::testing::{TestResult, TestStatus};

use crate::{go, istanbul, lcov, path_to_uri, CoverageAdapter, RunData};

/// Attach a source [`Location`] to each test by scanning the workspace for
/// `#[test]` functions and matching on the test's leaf name. Best-effort: names
/// it can't resolve are left without a location.
fn attach_test_locations(results: &mut [TestResult], root: &Path) {
    if results.is_empty() {
        return;
    }
    let locations = resolve_test_locations(root);
    for r in results.iter_mut() {
        let leaf = r.name.rsplit("::").next().unwrap_or(&r.name);
        if let Some(candidates) = locations.get(leaf) {
            if let Some((rel, line)) = choose_location(&r.name, candidates) {
                r.location = Some(Location {
                    uri: path_to_uri(root, rel),
                    range: Range::whole_line(*line),
                });
            }
        }
    }
}

/// When a leaf test name resolves to several files, prefer one whose file stem
/// appears as a module segment of the full test name (e.g. `engine::tests::foo`
/// → `engine.rs`); otherwise take the first.
fn choose_location<'a>(name: &str, candidates: &'a [(String, u32)]) -> Option<&'a (String, u32)> {
    if candidates.len() == 1 {
        return candidates.first();
    }
    let segments: Vec<&str> = name.split("::").collect();
    candidates
        .iter()
        .find(|(path, _)| {
            let stem = Path::new(path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            segments.iter().any(|seg| *seg == stem)
        })
        .or_else(|| candidates.first())
}

/// Map each `#[test]` function's leaf name to the source files + 0-based lines
/// where it is defined. Walks `.rs` files, skipping build/output dirs.
fn resolve_test_locations(root: &Path) -> HashMap<String, Vec<(String, u32)>> {
    fn walk(dir: &Path, root: &Path, map: &mut HashMap<String, Vec<(String, u32)>>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                if !is_ignored_dir(&path) {
                    walk(&path, root, map);
                }
            } else if ft.is_file() && path.extension().map(|e| e == "rs").unwrap_or(false) {
                let Ok(content) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned();
                // After a `#[test]`-style attribute, the next `fn NAME` within a
                // few lines (past other attributes) is that test's definition.
                let mut look = 0u32;
                for (i, line) in content.lines().enumerate() {
                    let t = line.trim_start();
                    if t.starts_with("#[") && t.contains("test]") {
                        look = 6;
                        continue;
                    }
                    if look > 0 {
                        if let Some(name) = parse_fn_name(t) {
                            map.entry(name).or_default().push((rel.clone(), i as u32));
                            look = 0;
                        } else if !t.starts_with("#[") && !t.is_empty() {
                            look -= 1;
                        }
                    }
                }
            }
        }
    }
    let mut map = HashMap::new();
    walk(root, root, &mut map);
    map
}

/// Extract the function name from a line like `fn foo(`, `pub async fn foo<T>(`.
fn parse_fn_name(line: &str) -> Option<String> {
    let idx = line.find("fn ")?;
    // Require `fn ` to start a token (line start or preceded by whitespace).
    if idx != 0 && !line[..idx].ends_with(char::is_whitespace) {
        return None;
    }
    let rest = &line[idx + 3..];
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

/// Where the Rust adapter stashes parsed test results between runs. Lives under
/// `target/` (ignored by the freshness walk and the artifact search) so it is
/// reused on the fast path without ever making coverage look stale.
const TEST_RESULTS_SIDECAR: &str = "target/.csp-test-results.json";

/// Parse libtest's human-readable output (the lines cargo-llvm-cov prints while
/// running the suite) into [`TestResult`]s. Each test prints
/// `test <name> ... ok|FAILED|ignored`. `run_id` is stamped on every result.
fn parse_cargo_test_output(stdout: &str, run_id: &RunId) -> Vec<TestResult> {
    let mut out = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("test ") else {
            continue;
        };
        // Skip the summary line ("test result: ok. ...").
        let Some((name, status)) = rest.rsplit_once(" ... ") else {
            continue;
        };
        let status = match status.trim() {
            "ok" => TestStatus::Pass,
            s if s.starts_with("FAILED") => TestStatus::Fail,
            s if s.starts_with("ignored") => TestStatus::Skip,
            _ => continue,
        };
        let name = name.trim().to_string();
        out.push(TestResult {
            id: name.clone(),
            name,
            status,
            run_id: run_id.clone(),
            message: None,
            location: None,
            duration_ms: None,
        });
    }
    out
}

/// Read the test-results sidecar (if present), stamping the current `run_id`.
fn read_test_results(root: &Path, run_id: &RunId) -> Vec<TestResult> {
    let Ok(bytes) = std::fs::read(root.join(TEST_RESULTS_SIDECAR)) else {
        return Vec::new();
    };
    let Ok(mut results) = serde_json::from_slice::<Vec<TestResult>>(&bytes) else {
        return Vec::new();
    };
    for r in &mut results {
        r.run_id = run_id.clone();
    }
    results
}

/// Whether the server may run the project's coverage tool to generate a missing
/// artifact. On by default; set `CSP_NO_AUTORUN` to make adapters report-only.
pub fn autorun_enabled() -> bool {
    std::env::var_os("CSP_NO_AUTORUN").is_none()
}

/// Whether the server may install a missing coverage tool itself (e.g.
/// `cargo install cargo-llvm-cov`). On by default; set `CSP_NO_AUTOINSTALL` to
/// require the tool to be installed already.
pub fn autoinstall_enabled() -> bool {
    std::env::var_os("CSP_NO_AUTOINSTALL").is_none()
}

/// True if `program subcommand --version` (or `program --version`) succeeds,
/// i.e. the tool is installed and runnable.
fn tool_available(program: &str, probe_args: &[&str]) -> bool {
    Command::new(program)
        .args(probe_args)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Run a coverage tool in `root`, returning a helpful error if it is not
/// installed or exits non-zero.
fn run_tool(root: &Path, program: &str, args: &[&str]) -> anyhow::Result<()> {
    let output = Command::new(program)
        .args(args)
        .current_dir(root)
        .output()
        .with_context(|| format!("could not run `{program}` (is it installed and on PATH?)"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "`{program} {}` failed: {}",
            args.join(" "),
            stderr.trim()
        );
    }
    Ok(())
}

/// Ensure the `cargo llvm-cov` subcommand is available, installing it (and its
/// required `llvm-tools` component) when missing and auto-installing is enabled.
/// The install compiles from source, so it is slow but one-time.
fn ensure_cargo_llvm_cov() -> anyhow::Result<()> {
    if tool_available("cargo", &["llvm-cov", "--version"]) {
        return Ok(());
    }
    if !autoinstall_enabled() {
        anyhow::bail!(
            "cargo-llvm-cov is not installed (run `cargo install cargo-llvm-cov`, \
             or unset CSP_NO_AUTOINSTALL to let csp install it)"
        );
    }
    // cargo-llvm-cov needs the llvm-tools component; best-effort (a no-op when
    // rustup isn't the toolchain manager).
    let _ = Command::new("rustup")
        .args(["component", "add", "llvm-tools-preview"])
        .output();

    // Prefer a prebuilt binary the way polished LSP clients fetch prebuilt
    // servers: `cargo binstall` grabs the release artifact in seconds. Only fall
    // back to compiling from source (slow) when binstall isn't available.
    if tool_available("cargo", &["binstall", "-V"]) {
        let out = Command::new("cargo")
            .args(["binstall", "-y", "cargo-llvm-cov"])
            .output();
        if matches!(&out, Ok(o) if o.status.success())
            && tool_available("cargo", &["llvm-cov", "--version"])
        {
            return Ok(());
        }
    }

    let output = Command::new("cargo")
        .args(["install", "cargo-llvm-cov", "--locked"])
        .output()
        .context("could not run `cargo install cargo-llvm-cov`")?;
    if !output.status.success() {
        anyhow::bail!(
            "`cargo install cargo-llvm-cov` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// Locate `llvm-cov` and `llvm-profdata` (the tools cargo-llvm-cov drives) for a
/// non-rustup toolchain. Order: PATH, then an already-installed Homebrew `llvm`,
/// then Apple's `xcrun`, then — when auto-installing — `brew install llvm`.
/// Returns absolute paths to `(llvm-cov, llvm-profdata)`.
fn find_llvm_tools() -> Option<(String, String)> {
    if let (Some(c), Some(p)) = (which_tool("llvm-cov"), which_tool("llvm-profdata")) {
        return Some((c, p));
    }
    if let Some(pair) = brew_llvm_tools(false) {
        return Some(pair);
    }
    if let (Some(c), Some(p)) = (xcrun_find("llvm-cov"), xcrun_find("llvm-profdata")) {
        return Some((c, p));
    }
    if autoinstall_enabled() {
        if let Some(pair) = brew_llvm_tools(true) {
            return Some(pair);
        }
    }
    None
}

/// Absolute path of `name` on PATH, via `which`.
fn which_tool(name: &str) -> Option<String> {
    let out = Command::new("which").arg(name).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .filter(|s| !s.is_empty())
}

/// `xcrun -f <name>` — resolves tools in the active Apple toolchain.
fn xcrun_find(name: &str) -> Option<String> {
    let out = Command::new("xcrun").args(["-f", name]).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .filter(|s| !s.is_empty())
}

/// `(llvm-cov, llvm-profdata)` from a Homebrew `llvm` keg (binaries live under
/// `<prefix>/bin`, since the formula is keg-only). When `install` is true and the
/// keg is absent, runs `brew install llvm` first.
fn brew_llvm_tools(install: bool) -> Option<(String, String)> {
    if which_tool("brew").is_none() {
        return None;
    }
    let lookup = || {
        let out = Command::new("brew").args(["--prefix", "llvm"]).output().ok()?;
        if !out.status.success() {
            return None;
        }
        let prefix = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_owned());
        let cov = prefix.join("bin").join("llvm-cov");
        let profdata = prefix.join("bin").join("llvm-profdata");
        (cov.exists() && profdata.exists()).then(|| {
            (
                cov.to_string_lossy().into_owned(),
                profdata.to_string_lossy().into_owned(),
            )
        })
    };
    if let Some(pair) = lookup() {
        return Some(pair);
    }
    if install {
        let _ = Command::new("brew").args(["install", "llvm"]).output();
        return lookup();
    }
    None
}

/// Capabilities for a line+branch+function adapter (lcov / istanbul).
fn full_caps() -> ServerCapabilities {
    ServerCapabilities {
        coverage: CoverageCapabilities {
            line: true,
            branch: true,
            function: true,
        },
        test_results: false,
        test_mapping: false,
        quality_diagnostics: true,
        run_control: false,
    }
}

/// Return the first of `candidates` (relative to `root`) that exists.
fn first_existing(root: &Path, candidates: &[&str]) -> Option<PathBuf> {
    candidates
        .iter()
        .map(|c| root.join(c))
        .find(|p| p.exists())
}

/// Find a `*.info` LCOV file at the root or one level down in common dirs.
fn find_lcov(root: &Path) -> Option<PathBuf> {
    if let Some(p) = first_existing(
        root,
        &["lcov.info", "coverage/lcov.info", "target/llvm-cov/lcov.info"],
    ) {
        return Some(p);
    }
    // Fall back to any *.info directly under root.
    std::fs::read_dir(root).ok()?.flatten().find_map(|e| {
        let p = e.path();
        (p.extension().map(|x| x == "info").unwrap_or(false)).then_some(p)
    })
}

/// Return the artifact `find` locates, generating it via `adapter` when it is
/// missing or **stale** (older than some source file). A fresh artifact is
/// reused as-is. `hint` names the manual command for error messages.
fn ensure_artifact(
    adapter: &dyn CoverageAdapter,
    root: &Path,
    find: fn(&Path) -> Option<PathBuf>,
    hint: &str,
) -> anyhow::Result<PathBuf> {
    let existing = find(root);
    if let Some(p) = &existing {
        if artifact_is_fresh(p, root) {
            return Ok(p.clone());
        }
        // Stale: a source changed after the report was produced.
    }
    if !autorun_enabled() {
        // Report-only mode: prefer a stale artifact over no coverage at all.
        return existing
            .with_context(|| format!("no coverage artifact found (run `{hint}`, or unset CSP_NO_AUTORUN)"));
    }

    // Generate. A non-zero exit is commonly just failing tests — the tool still
    // writes coverage — so trust the artifact over the exit status: if a (newly
    // written) artifact is present, use it and ignore the generate error.
    let gen_result = adapter.generate(root);
    match find(root) {
        Some(p) => Ok(p),
        None => Err(gen_result
            .err()
            .unwrap_or_else(|| anyhow::anyhow!("`{hint}` ran but produced no coverage artifact"))),
    }
}

/// Whether `artifact` is at least as new as every source file under `root`, so
/// regeneration can be skipped. Build/output dirs are ignored, as is the
/// artifact itself. Conservatively treats an unreadable mtime as stale.
fn artifact_is_fresh(artifact: &Path, root: &Path) -> bool {
    let Ok(artifact_mtime) = artifact.metadata().and_then(|m| m.modified()) else {
        return false;
    };
    match newest_source_mtime(root, artifact) {
        Some(newest_src) => artifact_mtime >= newest_src,
        None => true, // nothing to be stale against
    }
}

/// Newest modification time among files under `root`, skipping build/output dirs
/// and `artifact`. Metadata-only walk (no file reads).
fn newest_source_mtime(root: &Path, artifact: &Path) -> Option<std::time::SystemTime> {
    fn walk(dir: &Path, artifact: &Path, newest: &mut Option<std::time::SystemTime>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if file_type.is_dir() {
                if !is_ignored_dir(&path) {
                    walk(&path, artifact, newest);
                }
            } else if file_type.is_file() && path != artifact {
                if let Ok(m) = entry.metadata().and_then(|md| md.modified()) {
                    if newest.is_none_or(|n| m > n) {
                        *newest = Some(m);
                    }
                }
            }
        }
    }
    let mut newest = None;
    walk(root, artifact, &mut newest);
    newest
}

/// Directories that never count as sources for freshness (build outputs, deps,
/// VCS, coverage reports).
fn is_ignored_dir(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|n| n.to_str()),
        Some(
            "target"
                | "node_modules"
                | ".git"
                | "coverage"
                | ".nyc_output"
                | "dist"
                | "build"
                | ".next"
                | ".turbo"
                | "vendor"
        )
    )
}

// --- Rust (cargo-llvm-cov, emits LCOV) ---

pub struct RustAdapter;

impl CoverageAdapter for RustAdapter {
    fn name(&self) -> &'static str {
        "cargo-llvm-cov"
    }

    fn detect(&self, root: &Path) -> bool {
        root.join("Cargo.toml").exists()
    }

    fn capabilities(&self) -> ServerCapabilities {
        // Rust also reports pass/fail from the same test run.
        ServerCapabilities {
            test_results: true,
            ..full_caps()
        }
    }

    fn generate(&self, root: &Path) -> anyhow::Result<()> {
        ensure_cargo_llvm_cov()?;

        // Runs the test suite under instrumentation and writes LCOV to the root.
        // `--ignore-run-fail` runs every test and still produces the report when
        // some fail — failing tests are normal and their coverage is just as
        // valid. (It implies no-fail-fast and can't be combined with it.)
        let mut cmd = Command::new("cargo");
        cmd.args([
            "llvm-cov",
            "--ignore-run-fail",
            "--lcov",
            "--output-path",
            "lcov.info",
        ])
        .current_dir(root);

        // cargo-llvm-cov needs `llvm-cov` + `llvm-profdata`. On a rustup toolchain
        // these come from the `llvm-tools-preview` component; on a non-rustup
        // toolchain (e.g. Homebrew Rust) that component doesn't exist, so we point
        // cargo-llvm-cov at an LLVM install via env instead — its documented
        // fallback. Only set env we found, leaving any caller-provided env intact.
        if std::env::var_os("LLVM_COV").is_none() || std::env::var_os("LLVM_PROFDATA").is_none() {
            if let Some((llvm_cov, llvm_profdata)) = find_llvm_tools() {
                cmd.env("LLVM_COV", llvm_cov).env("LLVM_PROFDATA", llvm_profdata);
            }
        }

        let output = cmd
            .output()
            .context("could not run `cargo llvm-cov` (is cargo on PATH?)")?;
        if !output.status.success() {
            // A non-zero exit is usually just failing tests; cargo-llvm-cov still
            // writes coverage, which `ensure_artifact` will pick up. Only surface
            // an error when no artifact was produced. Test failures print to
            // stdout, so include both streams in the message.
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            anyhow::bail!(
                "`cargo llvm-cov` exited non-zero: {}",
                [stdout.trim(), stderr.trim()]
                    .iter()
                    .filter(|s| !s.is_empty())
                    .last()
                    .copied()
                    .unwrap_or("(no output)")
            );
        }

        // Stash parsed test results alongside coverage so `collect` can surface
        // them (and reuse them on the freshness fast path). run_id is stamped at
        // read time, so store with an empty placeholder.
        let results = parse_cargo_test_output(&String::from_utf8_lossy(&output.stdout), &String::new());
        let mut results = results;
        attach_test_locations(&mut results, root);
        if !results.is_empty() {
            if let Ok(json) = serde_json::to_vec(&results) {
                let _ = std::fs::write(root.join(TEST_RESULTS_SIDECAR), json);
            }
        }
        Ok(())
    }

    fn collect(&self, root: &Path, run_id: &RunId) -> anyhow::Result<RunData> {
        let path = ensure_artifact(self, root, find_lcov, "cargo llvm-cov --lcov")?;
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let mut data = RunData::from_coverage(run_id.clone(), lcov::parse(&content, root, run_id));
        data.results = read_test_results(root, run_id);
        Ok(data)
    }
}

// --- Go (go test -coverprofile) ---

pub struct GoAdapter;

impl GoAdapter {
    fn find_profile(root: &Path) -> Option<PathBuf> {
        first_existing(root, &["coverage.out", "cover.out", "coverage.txt"])
    }
}

impl CoverageAdapter for GoAdapter {
    fn name(&self) -> &'static str {
        "go"
    }

    fn detect(&self, root: &Path) -> bool {
        root.join("go.mod").exists()
    }

    fn capabilities(&self) -> ServerCapabilities {
        ServerCapabilities {
            coverage: CoverageCapabilities {
                line: true,
                branch: false,
                function: false,
            },
            quality_diagnostics: true,
            ..ServerCapabilities::default()
        }
    }

    fn generate(&self, root: &Path) -> anyhow::Result<()> {
        run_tool(
            root,
            "go",
            &["test", "-coverprofile=coverage.out", "./..."],
        )
    }

    fn collect(&self, root: &Path, run_id: &RunId) -> anyhow::Result<RunData> {
        let path = ensure_artifact(
            self,
            root,
            Self::find_profile,
            "go test -coverprofile=coverage.out ./...",
        )?;
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        Ok(RunData::from_coverage(
            run_id.clone(),
            go::parse(&content, root, run_id),
        ))
    }
}

// --- TypeScript / JavaScript (Istanbul JSON, or lcov fallback) ---

pub struct TsAdapter;

impl TsAdapter {
    fn find_istanbul(root: &Path) -> Option<PathBuf> {
        first_existing(
            root,
            &[
                "coverage/coverage-final.json",
                "coverage-final.json",
                ".nyc_output/coverage-final.json",
            ],
        )
    }
}

impl CoverageAdapter for TsAdapter {
    fn name(&self) -> &'static str {
        "istanbul"
    }

    fn detect(&self, root: &Path) -> bool {
        root.join("package.json").exists()
    }

    fn capabilities(&self) -> ServerCapabilities {
        full_caps()
    }

    fn generate(&self, root: &Path) -> anyhow::Result<()> {
        // Vitest with the Istanbul provider + json reporter writes
        // coverage/coverage-final.json, which `find_istanbul` then locates.
        run_tool(
            root,
            "npx",
            &[
                "--no-install",
                "vitest",
                "run",
                "--coverage",
                "--coverage.provider=istanbul",
                "--coverage.reporter=json",
            ],
        )
    }

    fn collect(&self, root: &Path, run_id: &RunId) -> anyhow::Result<RunData> {
        // Prefer the richer Istanbul JSON; reuse an LCOV report if one exists.
        if Self::find_istanbul(root).is_none() && find_lcov(root).is_some() {
            let path = find_lcov(root).unwrap();
            let content = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            return Ok(RunData::from_coverage(
                run_id.clone(),
                lcov::parse(&content, root, run_id),
            ));
        }
        let path = ensure_artifact(
            self,
            root,
            Self::find_istanbul,
            "vitest run --coverage --coverage.provider=istanbul",
        )?;
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let coverage = istanbul::parse(&content, root, run_id)
            .with_context(|| format!("parsing istanbul JSON {}", path.display()))?;
        Ok(RunData::from_coverage(run_id.clone(), coverage))
    }
}

// --- Generic LCOV fallback (any language) ---

pub struct GenericLcovAdapter;

impl CoverageAdapter for GenericLcovAdapter {
    fn name(&self) -> &'static str {
        "lcov"
    }

    fn detect(&self, root: &Path) -> bool {
        find_lcov(root).is_some()
    }

    fn capabilities(&self) -> ServerCapabilities {
        full_caps()
    }

    fn collect(&self, root: &Path, run_id: &RunId) -> anyhow::Result<RunData> {
        let path = find_lcov(root).context("no LCOV file found")?;
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        Ok(RunData::from_coverage(
            run_id.clone(),
            lcov::parse(&content, root, run_id),
        ))
    }
}

// --- Configured (from `.csp.toml`): run a user-specified command ---

/// Runs the exact command a project's `.csp.toml` specifies, then parses the
/// artifact it writes. This is the escape hatch for repos where auto-detection
/// runs too much — e.g. scoping a monorepo to one crate so the instrumented
/// build actually succeeds.
pub struct ConfiguredAdapter {
    command: Vec<String>,
    artifact: String,
    autorun: bool,
}

impl ConfiguredAdapter {
    pub fn new(command: Vec<String>, artifact: String, autorun: bool) -> Self {
        Self {
            command,
            artifact,
            autorun,
        }
    }

    fn artifact_path(&self, root: &Path) -> PathBuf {
        root.join(&self.artifact)
    }
}

impl CoverageAdapter for ConfiguredAdapter {
    fn name(&self) -> &'static str {
        "configured"
    }

    fn detect(&self, _root: &Path) -> bool {
        true
    }

    fn capabilities(&self) -> ServerCapabilities {
        ServerCapabilities {
            test_results: true,
            ..full_caps()
        }
    }

    fn generate(&self, root: &Path) -> anyhow::Result<()> {
        let (program, args) = self
            .command
            .split_first()
            .context("`.csp.toml` command is empty")?;
        let mut cmd = Command::new(program);
        cmd.args(args).current_dir(root);

        // If the configured command drives cargo-llvm-cov, supply the LLVM tools
        // the same way the Rust adapter does (non-rustup toolchains lack them).
        if self.command.iter().any(|a| a == "llvm-cov")
            && (std::env::var_os("LLVM_COV").is_none() || std::env::var_os("LLVM_PROFDATA").is_none())
        {
            if let Some((llvm_cov, llvm_profdata)) = find_llvm_tools() {
                cmd.env("LLVM_COV", llvm_cov).env("LLVM_PROFDATA", llvm_profdata);
            }
        }

        let output = cmd
            .output()
            .with_context(|| format!("could not run `{program}` (is it installed and on PATH?)"))?;

        // Best-effort test results from libtest-style stdout.
        let results = parse_cargo_test_output(&String::from_utf8_lossy(&output.stdout), &String::new());
        let mut results = results;
        attach_test_locations(&mut results, root);
        if !results.is_empty() {
            if let Ok(json) = serde_json::to_vec(&results) {
                let _ = std::fs::write(root.join(TEST_RESULTS_SIDECAR), json);
            }
        }

        // Trust the artifact over the exit code (failing tests still emit it).
        if !output.status.success() && !self.artifact_path(root).exists() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            anyhow::bail!(
                "`{}` failed: {}",
                self.command.join(" "),
                [stdout.trim(), stderr.trim()]
                    .iter()
                    .filter(|s| !s.is_empty())
                    .last()
                    .copied()
                    .unwrap_or("(no output)")
            );
        }
        Ok(())
    }

    fn collect(&self, root: &Path, run_id: &RunId) -> anyhow::Result<RunData> {
        let artifact = self.artifact_path(root);
        let need_gen = !(artifact.exists() && artifact_is_fresh(&artifact, root));
        let gen_err = if need_gen && self.autorun && autorun_enabled() {
            self.generate(root).err()
        } else {
            None
        };
        if !artifact.exists() {
            return Err(gen_err.unwrap_or_else(|| {
                anyhow::anyhow!("no coverage artifact at {}", artifact.display())
            }));
        }
        let content = std::fs::read_to_string(&artifact)
            .with_context(|| format!("reading {}", artifact.display()))?;
        let coverage = parse_artifact(&content, &self.artifact, root, run_id)?;
        let mut data = RunData::from_coverage(run_id.clone(), coverage);
        data.results = read_test_results(root, run_id);
        Ok(data)
    }
}

/// Parse a coverage artifact by its file extension: `.json` → Istanbul,
/// `.out`/`.txt` → Go profile, anything else → LCOV.
fn parse_artifact(
    content: &str,
    name: &str,
    root: &Path,
    run_id: &RunId,
) -> anyhow::Result<Vec<csp_core::coverage::FileCoverage>> {
    if name.ends_with(".json") {
        istanbul::parse(content, root, run_id)
    } else if name.ends_with(".out") || name.ends_with(".txt") {
        Ok(go::parse(content, root, run_id))
    } else {
        Ok(lcov::parse(content, root, run_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    fn set_mtime(path: &Path, secs: u64) {
        let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(UNIX_EPOCH + Duration::from_secs(secs)).unwrap();
    }

    #[test]
    fn resolves_test_locations_from_source() {
        let dir = std::env::temp_dir().join(format!("csp-loc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("src/math.rs"),
            "pub fn add() {}\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn adds() {}\n    #[test]\n    #[ignore]\n    fn skips() {}\n}\n",
        )
        .unwrap();

        let mut results = vec![
            TestResult {
                id: "tests::adds".into(),
                name: "tests::adds".into(),
                status: TestStatus::Pass,
                run_id: "r".into(),
                message: None,
                location: None,
                duration_ms: None,
            },
            TestResult {
                id: "tests::skips".into(),
                name: "tests::skips".into(),
                status: TestStatus::Skip,
                run_id: "r".into(),
                message: None,
                location: None,
                duration_ms: None,
            },
        ];
        attach_test_locations(&mut results, &dir);

        // `adds` is on 0-based line 4 (after the two attribute lines past `#[test]`).
        let adds = results[0].location.as_ref().expect("located");
        assert!(adds.uri.ends_with("src/math.rs"));
        assert_eq!(adds.range.start.line, 4);
        // `skips` has `#[test]` then `#[ignore]` then `fn` on line 7.
        assert_eq!(results[1].location.as_ref().unwrap().range.start.line, 7);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_command_selects_configured_adapter() {
        let dir = std::env::temp_dir().join(format!("csp-cfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A Cargo.toml would normally select RustAdapter; the .csp.toml command wins.
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        std::fs::write(
            dir.join(".csp.toml"),
            "command = \"echo hi\"\nartifact = \"cov.info\"\n",
        )
        .unwrap();
        let adapter = crate::select(&dir, None).expect("an adapter");
        assert_eq!(adapter.name(), "configured");
        assert!(adapter.capabilities().test_results);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_libtest_output_into_results() {
        let stdout = "\
running 4 tests
test tests::passes ... ok
test tests::also_passes ... ok
test tests::fails ... FAILED
test tests::skipped ... ignored

test result: FAILED. 2 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out
";
        let results = parse_cargo_test_output(stdout, &"run-1".to_string());
        assert_eq!(results.len(), 4, "summary line must not be counted");
        let by = |s: TestStatus| results.iter().filter(|r| r.status == s).count();
        assert_eq!(by(TestStatus::Pass), 2);
        assert_eq!(by(TestStatus::Fail), 1);
        assert_eq!(by(TestStatus::Skip), 1);
        assert_eq!(results[0].name, "tests::passes");
        assert_eq!(results[0].run_id, "run-1");
    }

    #[test]
    fn freshness_tracks_source_vs_artifact_mtime() {
        let dir = std::env::temp_dir().join(format!("csp-fresh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let src = dir.join("src/lib.rs");
        let artifact = dir.join("lcov.info");
        std::fs::write(&src, "fn a() {}").unwrap();
        std::fs::write(&artifact, "SF:src/lib.rs\nend_of_record\n").unwrap();

        // Artifact newer than the source -> fresh (skip regeneration).
        set_mtime(&src, 1_000);
        set_mtime(&artifact, 2_000);
        assert!(artifact_is_fresh(&artifact, &dir));

        // Source edited after the artifact -> stale (regenerate).
        set_mtime(&src, 3_000);
        assert!(!artifact_is_fresh(&artifact, &dir));

        // Files under ignored dirs (e.g. target/) never make it stale.
        std::fs::create_dir_all(dir.join("target")).unwrap();
        let built = dir.join("target/out");
        std::fs::write(&built, "x").unwrap();
        set_mtime(&built, 9_000);
        set_mtime(&src, 1_000);
        set_mtime(&artifact, 2_000);
        assert!(artifact_is_fresh(&artifact, &dir), "target/ must be ignored");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
