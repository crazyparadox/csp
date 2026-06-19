import * as vscode from "vscode";

import { CoverageSummary, FileCoverage } from "./protocol";

// Client-side cache of the coverage the server has pushed.
//
// Keyed by the document's *filesystem path*, not the raw URI string: the server
// may normalise paths (percent-encoding, symlink resolution) so its `uri` won't
// byte-match `document.uri.toString()`. `Uri.parse(serverUri).fsPath` decodes
// back to a path we can compare against any open document (spec/csp.md §1.1).
export class CoverageStore implements vscode.Disposable {
  private readonly byPath = new Map<string, FileCoverage>();
  private workspaceSummary: CoverageSummary | undefined;

  private readonly onDidChangeEmitter = new vscode.EventEmitter<string>();
  /** Fires with the fsPath whose coverage changed. */
  readonly onDidChange = this.onDidChangeEmitter.event;

  /** Normalise any URI (server or editor) to a stable cache key. */
  static keyFor(uri: string): string {
    try {
      return vscode.Uri.parse(uri).fsPath;
    } catch {
      return uri;
    }
  }

  update(coverage: FileCoverage): void {
    const key = CoverageStore.keyFor(coverage.uri);
    this.byPath.set(key, coverage);
    this.onDidChangeEmitter.fire(key);
  }

  get(documentUri: vscode.Uri): FileCoverage | undefined {
    return this.byPath.get(documentUri.fsPath);
  }

  /** The server's original URI string for a document, if we've seen coverage. */
  serverUriFor(documentUri: vscode.Uri): string | undefined {
    return this.byPath.get(documentUri.fsPath)?.uri;
  }

  /** Drop all cached coverage (e.g. on server restart) and repaint listeners. */
  clear(): void {
    const keys = [...this.byPath.keys()];
    this.byPath.clear();
    this.workspaceSummary = undefined;
    for (const key of keys) {
      this.onDidChangeEmitter.fire(key);
    }
  }

  setWorkspaceSummary(summary: CoverageSummary | undefined): void {
    this.workspaceSummary = summary;
  }

  getWorkspaceSummary(): CoverageSummary | undefined {
    return this.workspaceSummary;
  }

  dispose(): void {
    this.onDidChangeEmitter.dispose();
    this.byPath.clear();
  }
}
