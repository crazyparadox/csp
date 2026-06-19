# CSP Coverage — VS Code extension

A reference **Coverage Server Protocol** client for VS Code. It spawns a
`csp-server` over stdio, speaks the CSP lifecycle, and renders the coverage the
server pushes as **gutter markers**, an **overview-ruler** heatline, a
**status-bar** percentage, and **quality diagnostics** in the Problems panel.

This is the editor-side counterpart to the `csp-cli` debug client: where the CLI
prints a coverage table, this paints it into the editor.

```
 csp-server  ──stdio JSON-RPC──▶  this extension
   publishCoverage            ▶   green/red gutter bars
   publishQualityDiagnostics  ▶   Problems panel ("line not covered")
   runStateChanged.summary    ▶   status bar  "▣ 72%"
 didOpen / didChange          ◀   stale (grey) gutter after edits
```

## How it works

1. On activation the extension starts one `csp-server` per workspace folder
   (`--root <folder>`), runs `initialize` → `initialized`, and listens.
2. The server auto-collects coverage and pushes one `csp/publishCoverage` per
   file. The extension caches these and paints the active editor.
3. Editing a file sends `csp/didChange`; the server re-pushes that file with
   `stale: true`, which the extension renders as grey bars so green gutters never
   lie after an edit.

URIs are matched by **decoded filesystem path** (`Uri.fsPath`), not raw string,
so percent-encoding or path normalisation on the server side never breaks the
mapping (see `spec/csp.md` §1.1).

## Requirements

A `csp-server` binary. The extension finds one in this order:

1. `csp.serverPath` setting, if set.
2. `<workspace>/target/{release,debug}/csp-server`.
3. `<csp-repo>/target/{release,debug}/csp-server` (dev: this extension lives in
   the CSP repo).
4. `csp-server` on `PATH`.

Build the reference server with:

```sh
cargo build -p csp-server          # debug
cargo build -p csp-server --release
```

## Develop / run

```sh
cd examples/csp-extension
npm install
npm run build        # or: npm run watch
```

Then press **F5** ("Run CSP Extension"). It opens `examples/go-sample` in an
Extension Development Host with gutters lit up. Try other samples by editing the
folder argument in `.vscode/launch.json`, or just open any Rust/Go/TS project the
server supports.

## Settings

| Setting             | Default  | Description                                                        |
| ------------------- | -------- | ------------------------------------------------------------------ |
| `csp.serverPath`    | `""`     | Path to `csp-server`; empty = auto-discover (see above).           |
| `csp.framework`     | `"auto"` | Force an adapter: `cargo-llvm-cov` / `go` / `istanbul` / `lcov`.   |
| `csp.enableGutters` | `true`   | Render covered/uncovered gutter markers.                           |
| `csp.noAutorun`     | `false`  | Set `CSP_NO_AUTORUN`: report an existing artifact, don't run tests. |

## Commands

- **CSP: Restart Coverage Server**
- **CSP: Toggle Coverage Gutters**
- **CSP: Show Server Log**

## Limitations (v1)

- The client can't drive runs (`runControl` is `false` in CSP v1); the server
  owns collection. Re-run by saving/editing or via **CSP: Restart**.
- If a workspace is opened through a symlink, the server's canonicalised paths
  may differ from the editor's; coverage for those files won't match.
- The status bar shows the most recent folder's workspace summary.
