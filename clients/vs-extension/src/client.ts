import { ChildProcessWithoutNullStreams, spawn } from "node:child_process";
import * as fs from "node:fs";
import * as path from "node:path";

import * as vscode from "vscode";
import {
  createMessageConnection,
  MessageConnection,
  StreamMessageReader,
  StreamMessageWriter,
} from "vscode-jsonrpc/node";

import { CoverageStore } from "./coverageStore";
import {
  FileCoverage,
  InitializeParams,
  InitializeResult,
  Method,
  PublishTestResultsParams,
  RunStateChangedParams,
} from "./protocol";

interface QualityDiagnostic {
  range: {
    start: { line: number; character: number };
    end: { line: number; character: number };
  };
  severity: "error" | "warning" | "information" | "hint";
  message: string;
  source?: string;
  code?: string;
}

interface PublishQualityDiagnosticsParams {
  uri: string;
  diagnostics: QualityDiagnostic[];
}

const SEVERITY: Record<string, vscode.DiagnosticSeverity> = {
  error: vscode.DiagnosticSeverity.Error,
  warning: vscode.DiagnosticSeverity.Warning,
  information: vscode.DiagnosticSeverity.Information,
  hint: vscode.DiagnosticSeverity.Hint,
};

// Owns one `csp-server` child process and its JSON-RPC connection, for one
// workspace folder. Routes the server's pushes into the shared store, the
// diagnostics collection, and the status callback.
export class CspClient implements vscode.Disposable {
  private child: ChildProcessWithoutNullStreams | undefined;
  private connection: MessageConnection | undefined;
  private disposed = false;

  constructor(
    private readonly context: vscode.ExtensionContext,
    private readonly folder: vscode.WorkspaceFolder,
    private readonly store: CoverageStore,
    private readonly diagnostics: vscode.DiagnosticCollection,
    private readonly log: vscode.OutputChannel,
    private readonly onRunState: (params: RunStateChangedParams) => void,
  ) {}

  async start(): Promise<void> {
    const config = vscode.workspace.getConfiguration("csp", this.folder.uri);
    const serverPath = this.resolveServerPath(config.get<string>("serverPath", ""));
    if (!serverPath) {
      this.log.appendLine(
        `[csp] no csp-server binary found for ${this.folder.name}. ` +
          `Set "csp.serverPath" or build the server (cargo build -p csp-server).`,
      );
      return;
    }

    const args = ["--root", this.folder.uri.fsPath];
    const framework = config.get<string>("framework", "auto");
    if (framework && framework !== "auto") {
      args.push("--framework", framework);
    }

    const env = { ...process.env };
    if (config.get<boolean>("noAutorun", false)) {
      env.CSP_NO_AUTORUN = "1";
    }

    this.log.appendLine(`[csp] spawning ${serverPath} ${args.join(" ")}`);
    let child: ChildProcessWithoutNullStreams;
    try {
      child = spawn(serverPath, args, { cwd: this.folder.uri.fsPath, env });
    } catch (err) {
      this.log.appendLine(`[csp] failed to spawn server: ${String(err)}`);
      return;
    }
    this.child = child;

    child.on("error", (err) => this.log.appendLine(`[csp] server error: ${err.message}`));
    child.stderr.on("data", (buf: Buffer) =>
      this.log.append(buf.toString().replace(/^/gm, "[server] ")),
    );
    child.on("exit", (code, signal) => {
      if (!this.disposed) {
        this.log.appendLine(`[csp] server exited (code=${code}, signal=${signal})`);
      }
    });

    const connection = createMessageConnection(
      new StreamMessageReader(child.stdout),
      new StreamMessageWriter(child.stdin),
    );
    this.connection = connection;

    connection.onNotification(Method.PublishCoverage, (params: FileCoverage) => {
      this.store.update(params);
    });
    connection.onNotification(Method.RunStateChanged, (params: RunStateChangedParams) => {
      if (params.summary) {
        this.store.setWorkspaceSummary(params.summary);
      }
      if (params.state === "errored" && params.message) {
        this.log.appendLine(`[csp] run errored: ${params.message}`);
      }
      this.onRunState(params);
    });
    connection.onNotification(
      Method.PublishQualityDiagnostics,
      (params: PublishQualityDiagnosticsParams) => this.applyDiagnostics(params),
    );
    connection.onNotification(
      Method.PublishTestResults,
      (params: PublishTestResultsParams) => this.logTestResults(params),
    );
    connection.onClose(() => {
      if (!this.disposed) {
        this.log.appendLine("[csp] connection closed");
      }
    });

    connection.listen();

    try {
      const initParams: InitializeParams = {
        rootUri: this.folder.uri.toString(),
        capabilities: { coverageGutter: true, testResults: true, staleness: true },
        framework: framework === "auto" ? null : framework,
      };
      const result = (await connection.sendRequest(
        Method.Initialize,
        initParams,
      )) as InitializeResult;
      this.log.appendLine(`[csp] connected: ${result.serverInfo ?? "unknown server"}`);
      connection.sendNotification(Method.Initialized);
    } catch (err) {
      this.log.appendLine(`[csp] initialize failed: ${String(err)}`);
    }
  }

  /** Broadcast a document-sync notification; ignored by the server if unknown. */
  notify(method: string, params: unknown): void {
    this.connection?.sendNotification(method, params);
  }

  private logTestResults(params: PublishTestResultsParams): void {
    const count = (s: string) => params.results.filter((r) => r.status === s).length;
    this.log.appendLine(
      `[csp] tests: ${count("pass")} passed, ${count("fail")} failed, ${count("skip")} skipped`,
    );
    for (const r of params.results) {
      if (r.status === "fail") {
        this.log.appendLine(`[csp]   FAIL ${r.name}${r.message ? `: ${r.message}` : ""}`);
      }
    }
  }

  private applyDiagnostics(params: PublishQualityDiagnosticsParams): void {
    const fsPath = CoverageStore.keyFor(params.uri);
    const items = params.diagnostics.map((d) => {
      const range = new vscode.Range(
        d.range.start.line,
        d.range.start.character,
        d.range.end.line,
        d.range.end.character,
      );
      const diag = new vscode.Diagnostic(
        range,
        d.message,
        SEVERITY[d.severity] ?? vscode.DiagnosticSeverity.Hint,
      );
      diag.source = d.source ?? "csp";
      if (d.code) {
        diag.code = d.code;
      }
      return diag;
    });
    this.diagnostics.set(vscode.Uri.file(fsPath), items);
  }

  private resolveServerPath(configured: string): string | undefined {
    const exe = process.platform === "win32" ? "csp-server.exe" : "csp-server";
    const candidates: string[] = [];
    if (configured) {
      candidates.push(configured);
    }
    const workspaceRoot = this.folder.uri.fsPath;
    candidates.push(
      path.join(workspaceRoot, "target", "release", exe),
      path.join(workspaceRoot, "target", "debug", exe),
    );
    // The extension ships inside the CSP repo (<repo>/examples/csp-extension);
    // fall back to a server built there during development.
    const repoRoot = path.resolve(this.context.extensionUri.fsPath, "..", "..");
    candidates.push(
      path.join(repoRoot, "target", "release", exe),
      path.join(repoRoot, "target", "debug", exe),
    );

    for (const candidate of candidates) {
      try {
        if (fs.existsSync(candidate) && fs.statSync(candidate).isFile()) {
          return candidate;
        }
      } catch {
        // ignore and try the next candidate
      }
    }
    // Last resort: rely on PATH resolution by the OS.
    return configured || exe;
  }

  dispose(): void {
    this.disposed = true;
    try {
      this.connection?.sendNotification(Method.Exit);
    } catch {
      // connection may already be down
    }
    this.connection?.dispose();
    this.child?.kill();
  }
}
