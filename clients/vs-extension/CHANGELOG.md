# Change Log

## 0.1.0

Initial release.

- Coverage gutters: continuous green/red bars (Git dirty-diff style) with
  per-line hit-count hovers.
- Stale (grey) markers after edits, driven by CSP document sync.
- Status-bar workspace coverage percentage, colored by level, with an action menu.
- Quality diagnostics for uncovered lines in the Problems panel.
- "Refresh Coverage" command/toolbar action (CSP `run`).
- Auto-discovery of the `csp-server` binary; per-workspace-folder servers.
- Adapters: Rust (`cargo-llvm-cov`), Go, JS/TS (Istanbul), and generic lcov.
