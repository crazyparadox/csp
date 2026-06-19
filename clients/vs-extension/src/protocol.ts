// Wire types and method names for the Coverage Server Protocol (CSP), mirroring
// the Rust `csp-core` crate. Enums serialize as lower-case strings and structs
// use camelCase field names (see spec/csp.md §1).

export const Method = {
  // Lifecycle (shared with LSP).
  Initialize: "initialize",
  Initialized: "initialized",
  Shutdown: "shutdown",
  Exit: "exit",
  // Document sync (client → server notifications).
  DidOpen: "csp/didOpen",
  DidChange: "csp/didChange",
  DidClose: "csp/didClose",
  // Reporting (server → client notifications).
  PublishCoverage: "csp/publishCoverage",
  PublishTestResults: "csp/publishTestResults",
  PublishQualityDiagnostics: "csp/publishQualityDiagnostics",
  RunStateChanged: "csp/runStateChanged",
  // Queries (client → server requests).
  Coverage: "csp/coverage",
  Summary: "csp/summary",
  // Run control: ask the server to (re-)collect coverage now.
  Run: "csp/run",
} as const;

export interface Position {
  line: number;
  character: number;
}

export interface Range {
  start: Position;
  end: Position;
}

export interface ClientCapabilities {
  coverageGutter: boolean;
  testResults: boolean;
  staleness: boolean;
}

export interface InitializeParams {
  rootUri: string;
  capabilities: ClientCapabilities;
  framework?: string | null;
}

export interface CoverageCapabilities {
  line: boolean;
  branch: boolean;
  function: boolean;
}

export interface ServerCapabilities {
  coverage: CoverageCapabilities;
  testResults: boolean;
  testMapping: boolean;
  qualityDiagnostics: boolean;
  runControl: boolean;
}

export interface InitializeResult {
  capabilities: ServerCapabilities;
  serverInfo?: string;
}

export interface LineCoverage {
  line: number;
  hits: number;
}

export interface BranchCoverage {
  range: Range;
  arms: number[];
}

export interface FunctionCoverage {
  name: string;
  range: Range;
  hits: number;
}

export interface CoverageSummary {
  linesCovered: number;
  linesTotal: number;
  branchesCovered: number;
  branchesTotal: number;
  functionsCovered: number;
  functionsTotal: number;
}

// Params of `csp/publishCoverage` — a FileCoverage flattened to the top level.
export interface FileCoverage {
  uri: string;
  runId: string;
  version?: number;
  stale: boolean;
  lines: LineCoverage[];
  branches?: BranchCoverage[];
  functions?: FunctionCoverage[];
  summary: CoverageSummary;
}

export type RunState = "running" | "finished" | "errored";

export interface RunStateChangedParams {
  runId: string;
  state: RunState;
  message?: string;
  summary?: CoverageSummary;
}

export interface Location {
  uri: string;
  range: Range;
}

export type TestStatus = "pass" | "fail" | "skip";

export interface TestResult {
  id: string;
  name: string;
  status: TestStatus;
  runId: string;
  message?: string;
  location?: Location;
  durationMs?: number;
}

export interface PublishTestResultsParams {
  runId: string;
  results: TestResult[];
}

export interface DidOpenParams {
  uri: string;
  version: number;
}

export interface DidChangeParams {
  uri: string;
  version: number;
}

export interface DidCloseParams {
  uri: string;
}
