# CSP — Coverage Server Protocol

CSP is to **test quality** what LSP is to language intelligence: a standard,
language-agnostic, JSON-RPC protocol that lets any editor see **test coverage**,
**test results**, and **quality diagnostics** from any test tool, by speaking to a
*quality server*.

Implement coverage gutters / a test panel / quality diagnostics in your editor
**once**, and every CSP server — Rust, Go, TypeScript, … — lights them up.

> **v1: the server auto-collects, the client just observes.** Open a file and the
> server reuses an existing coverage artifact or **runs the project's coverage tool
> itself** (`cargo llvm-cov`, `go test -coverprofile`, `vitest --coverage`) — no
> manual step. If the Rust tool is missing it even **auto-installs `cargo-llvm-cov`**
> (one-time, compiles from source). The client can't drive runs yet (`runControl` is
> `false`). Opt out with `CSP_NO_AUTORUN` (report-only) or `CSP_NO_AUTOINSTALL` (don't
> install tools). See the spec.

## Layout

```
spec/
  csp.md            # the specification (start here)
  versioning.md     # compatibility policy
  schema/*.json     # JSON Schema per message, generated from csp-core
crates/
  csp-core/         # source of truth: wire types + JSON-RPC framing + schema gen
  csp-adapters/     # CoverageAdapter impls (rust llvm-cov, go cover, ts istanbul, lcov)
  csp-server/       # the reference server binary (stdio JSON-RPC)
clients/
  cli/              # debug client: spawn a server and print coverage
examples/           # tiny rust/go/ts projects with tests, for end-to-end checks
```

## Quick start

```sh
# build everything
cargo build

# run the unit tests + regenerate the JSON schemas
cargo test
cargo run -p csp-core --example gen_schema

# drive the reference server against a sample project
cargo run -p csp-cli -- --root examples/rust-sample
```

## How it fits together

```
   editor (LSP-style client)            quality server (this repo)
   ───────────────────────             ──────────────────────────
   csp/didOpen / didChange    ───────▶  tracks open docs + versions
                              ◀───────  csp/publishCoverage  (per file)
                              ◀───────  csp/publishTestResults
                              ◀───────  csp/publishQualityDiagnostics
   csp/coverage / summary     ◀──────▶  on-demand queries
   csp/testsForRange          ◀──────▶  test ↔ code mapping
```

The reference client lives in the **neumann** IDE (`../neumann`), mirroring its
existing LSP integration: a `csp.rs` manager spawns servers over stdio and bridges
their pushes onto neumann's event channel, where a coverage store drives gutter
markers and a Test/Coverage panel.

See [`spec/csp.md`](spec/csp.md) for the full protocol.
