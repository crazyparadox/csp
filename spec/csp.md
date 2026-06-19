# Coverage Server Protocol (CSP) — Specification v0.1

CSP is a protocol that does for **test quality** what the Language Server Protocol
(LSP) does for language intelligence. A *quality server* analyses a project's tests
and coverage and reports, over a standard wire protocol, three things an editor
cares about:

1. **Coverage** — which lines / branches / functions were executed, and how often.
2. **Test results** — which tests passed, failed, or were skipped, and where.
3. **Quality diagnostics** — coverage gaps, flaky tests, and similar findings,
   shaped like LSP diagnostics so existing problem UIs render them.

By standardising this, an editor implements coverage gutters, a test panel, and
quality diagnostics **once**, and any CSP server — for Rust, Go, TypeScript, or
anything else — lights them up.

> **Status: client-observational; the server auto-collects.** A *client* cannot
> drive test runs in v1 — the `runControl` capability is `false`, and the
> client-driven run-control methods (`csp/run`, `csp/cancel`, `csp/runProgress`)
> are reserved for a future version. The *server*, however, is responsible for
> producing coverage: the reference server reuses an existing coverage artifact
> when present and otherwise **runs the project's coverage tool itself**
> (`cargo llvm-cov`, `go test -coverprofile`, `vitest --coverage`) before pushing
> results, so the editor shows coverage without the user generating anything by
> hand. Server auto-collection can be disabled with `CSP_NO_AUTORUN`.

---

## 1. Relationship to LSP

CSP is intentionally a near-twin of LSP so that an editor that already speaks LSP
can add CSP with minimal new machinery, and so the two can run **side by side** on
the same documents.

| Concern            | CSP                                                            |
| ------------------ | ------------------------------------------------------------- |
| Transport          | JSON-RPC 2.0 with `Content-Length` framing (identical to LSP) |
| Base types         | `Position`, `Range`, `Location`, `DocumentUri` — same shape as LSP |
| Lifecycle          | `initialize` / `initialized` / `shutdown` / `exit`            |
| Capability model   | Client and server exchange capabilities in `initialize`       |
| Method namespace   | `csp/*` (so it never collides with LSP's `textDocument/*`)    |

The one deliberate divergence: CSP enums serialize as **lower-case strings**
(`"error"`, `"pass"`) rather than LSP's integers, for readable, schema-friendly
JSON. `DiagnosticSeverity` maps 1:1 to LSP: `error→1`, `warning→2`,
`information→3`, `hint→4`.

### 1.1 Document URIs

Every `uri` on the wire is an RFC 3986 `file://` URI with the path **percent-encoded**
(`A-Z a-z 0-9 - . _ ~` and `/` preserved, every other byte escaped as `%XX` over
its UTF-8 bytes) — identical to how LSP and VS Code emit `DocumentUri`. A client
**must** compare URIs by their decoded filesystem path, not by raw string equality,
because a server may normalise paths (e.g. resolve symlinks) before encoding them.

---

## 2. Transport

Identical to LSP. Each message is a UTF-8 JSON-RPC 2.0 payload preceded by an HTTP-style
header block terminated by `\r\n\r\n`:

```
Content-Length: 124\r\n
\r\n
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{ ... }}
```

Only `Content-Length` is required; other headers are accepted and ignored. The
reference server speaks this over **stdio**. The transport is decoupled from the
data model, so an embedder may also carry CSP messages over an existing socket or
its own IPC bus (neumann, for instance, bridges them onto its WebSocket channel).

Message kinds (standard JSON-RPC 2.0):

- **Request** — has `id`, `method`, optional `params`; expects a **Response**.
- **Notification** — has `method`, optional `params`, **no** `id`; no reply.
- **Response** — has `id` and either `result` or `error`.

Error codes follow JSON-RPC: `-32601` MethodNotFound (returned for the reserved
run-control methods), `-32602` InvalidParams, `-32603` InternalError, etc.

---

## 3. Lifecycle

```
client                          server
  │  initialize (request)  ───────▶
  │  ◀─────────  initialize result
  │  initialized (notif)   ───────▶          ← server may now push
  │             … session …
  │  shutdown (request)    ───────▶
  │  ◀──────────────────  null result
  │  exit (notif)          ───────▶          ← process exits
```

### `initialize` (request)

Params: [`initialize_params.json`](./schema/initialize_params.json)

```jsonc
{
  "rootUri": "file:///home/me/project",
  "capabilities": { "coverageGutter": true, "testResults": true, "staleness": true },
  "framework": "cargo-llvm-cov"   // optional hint; server may auto-detect
}
```

Result: [`initialize_result.json`](./schema/initialize_result.json)

```jsonc
{
  "capabilities": {
    "coverage": { "line": true, "branch": true, "function": true },
    "testResults": true,
    "testMapping": true,
    "qualityDiagnostics": true,
    "runControl": false
  },
  "serverInfo": "csp-server 0.1.0 (rust/llvm-cov)"
}
```

A client **must** honour the server's advertised capabilities: do not call
`csp/testsForRange` unless `testMapping` is true, etc. A server **must** advertise
`runControl: false` in v1.

### `initialized` (notification)

Sent by the client after it has processed the initialize result. The server must
not push notifications before receiving it.

### `shutdown` (request) / `exit` (notification)

`shutdown` asks the server to stop work and returns `null`. `exit` then terminates
the process. Same semantics as LSP.

---

## 4. Document synchronization

So the server knows which files the user is looking at (and at which version, for
staleness), the client sends:

| Method           | Params                                                |
| ---------------- | ----------------------------------------------------- |
| `csp/didOpen`    | [`did_open_params.json`](./schema/did_open_params.json) — `{ uri, version }` |
| `csp/didChange`  | [`did_change_params.json`](./schema/did_change_params.json) — `{ uri, version }` |
| `csp/didClose`   | [`did_close_params.json`](./schema/did_close_params.json) — `{ uri }` |

These are notifications. `version` is the editor's document version (the same
integer LSP uses). The server uses it to decide whether previously computed
coverage is **stale** (see §6).

---

## 5. Reporting methods (server → client notifications)

These pushes are the heart of v1.

### `csp/publishCoverage`

Params: [`publish_coverage_params.json`](./schema/publish_coverage_params.json) —
a `FileCoverage` ([`file_coverage.json`](./schema/file_coverage.json)) flattened
to the top level. **One notification per file**, so the client updates
incrementally as a run completes rather than waiting for one giant payload.

```jsonc
{
  "uri": "file:///home/me/project/src/math.rs",
  "runId": "run-2026-06-19T10:00:00Z",
  "version": 7,
  "stale": false,
  "lines": [ { "line": 9, "hits": 3 }, { "line": 10, "hits": 0 } ],
  "branches": [ { "range": { "start": {"line":9,"character":4}, "end": {"line":9,"character":20} }, "arms": [3, 0] } ],
  "functions": [ { "name": "add", "range": { "start": {"line":8,"character":0}, "end": {"line":8,"character":20} }, "hits": 3 } ],
  "summary": { "linesCovered": 1, "linesTotal": 2, "branchesCovered": 1, "branchesTotal": 2, "functionsCovered": 1, "functionsTotal": 1 }
}
```

### `csp/publishTestResults`

Params: [`publish_test_results_params.json`](./schema/publish_test_results_params.json)
— `{ runId, results: TestResult[] }`. Each `TestResult`
([`test_result.json`](./schema/test_result.json)) carries `status`
(`pass`/`fail`/`skip`), an optional `message`, an optional `location` to surface
the failure at, and an optional `durationMs`.

### `csp/publishQualityDiagnostics`

Params: [`publish_quality_diagnostics_params.json`](./schema/publish_quality_diagnostics_params.json)
— `{ uri, diagnostics: Diagnostic[] }`. A **full replacement** set for the file
(same semantics as LSP `textDocument/publishDiagnostics`). Each diagnostic
([`diagnostic.json`](./schema/diagnostic.json)) uses `source` like `"csp.coverage"`
and `code` like `"uncovered-line"` so the UI can categorise and filter them.

### `csp/runStateChanged`

Params: [`run_state_changed_params.json`](./schema/run_state_changed_params.json)
— `{ runId, state, summary? }` where `state` is `running` / `finished` /
`errored`. A client typically shows a spinner on `running` and a workspace
coverage badge from the `summary` on `finished`.

`finished` is the run's completion barrier: the server **must** have emitted every
`csp/publishCoverage` (and `csp/publishTestResults` / `csp/publishQualityDiagnostics`)
for `runId` before sending `finished`. A client may therefore treat `finished` as
"all results for this run have arrived" — e.g. to clear coverage for files the run
did not report on, or to drop a loading state.

---

## 6. Staleness & versioning

Coverage is only meaningful relative to a source version, so CSP makes this
explicit instead of leaving it implicit:

- Every `FileCoverage` carries the `version` it was computed against (when known)
  and the `runId` of the producing run.
- When the client edits a file it sends `csp/didChange` with the new version. The
  server compares it to the version backing its current coverage and re-pushes the
  affected `FileCoverage` with `stale: true`.
- A client that advertised `staleness: true` should visually distinguish stale
  coverage (e.g. greyed gutter marks) rather than show it as authoritative.

This keeps green gutters from lying after an edit, without forcing a re-run.

---

## 7. Query methods (client → server requests)

For on-demand lookups and the test↔code mapping that makes CSP more than a
coverage dump. All gated by capabilities (§3).

| Method                | Params / Result                                                                 | Capability        |
| --------------------- | ------------------------------------------------------------------------------- | ----------------- |
| `csp/coverage`        | [params](./schema/coverage_params.json) `{ uri, range? }` → [result](./schema/coverage_result.json) `{ coverage? }` | `coverage.*`      |
| `csp/testsForRange`   | [params](./schema/tests_for_range_params.json) `{ uri, range }` → [result](./schema/tests_for_range_result.json) `{ tests }` | `testMapping`     |
| `csp/rangeForTest`    | [params](./schema/range_for_test_params.json) `{ testId }` → [result](./schema/range_for_test_result.json) `{ locations }` | `testMapping`     |
| `csp/summary`         | [params](./schema/summary_params.json) `{ uri? }` → [result](./schema/summary_result.json) `{ summary }` | `coverage.*`      |

- `csp/testsForRange` answers **"which tests cover this line?"** (code → test).
- `csp/rangeForTest` answers **"what code does this test cover?"** (test → code).
- `csp/summary` with no `uri` returns the whole-workspace rollup.

---

## 8. Reserved: run control (v-next)

Declared here so clients and servers can plan for it, but **not part of v1**. When
`runControl` is advertised `true` in a future version, a client may drive runs:

- `csp/run` `{ testIds?, uri? }` → start a run, returns a `runId`.
- `csp/cancel` `{ runId }` → cancel it.
- `csp/runProgress` (notification) → streaming progress, DAP-style.

Because run control executes project code, it must be gated behind explicit user
consent in the client. v1 servers return `MethodNotFound` (`-32601`) for these.

---

## 9. Versioning policy

See [`versioning.md`](./versioning.md). In short: the protocol is versioned
`MAJOR.MINOR.PATCH`; new optional capabilities and methods are minor, additive
changes; removing or repurposing a method is a major change. Clients and servers
negotiate features through capabilities, never through version sniffing.

---

## 10. Schemas

The `spec/schema/` directory holds a JSON Schema for every message, **generated
directly from the `csp-core` Rust types** (`cargo run -p csp-core --example
gen_schema`). They are the machine-checkable contract; this prose is the
explanation. If the two ever disagree, the schema — being generated from the
implementation — wins, and the prose is the bug.
