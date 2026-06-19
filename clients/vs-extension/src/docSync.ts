import * as vscode from "vscode";

import { CspClient } from "./client";
import { CoverageStore } from "./coverageStore";
import { DidChangeParams, DidCloseParams, DidOpenParams, Method } from "./protocol";

// Forwards editor document lifecycle to the servers so they can mark coverage
// stale after edits (spec/csp.md §6). Notifications are broadcast to every
// client; a server ignores URIs it doesn't track.
export class DocumentSync implements vscode.Disposable {
  private readonly disposables: vscode.Disposable[] = [];
  private readonly changeTimers = new Map<string, NodeJS.Timeout>();
  private static readonly DEBOUNCE_MS = 250;

  constructor(
    private readonly clients: () => CspClient[],
    private readonly store: CoverageStore,
  ) {
    this.disposables.push(
      vscode.workspace.onDidOpenTextDocument((doc) => this.open(doc)),
      vscode.workspace.onDidChangeTextDocument((e) => this.change(e.document)),
      vscode.workspace.onDidCloseTextDocument((doc) => this.close(doc)),
    );
    // Announce already-open documents.
    for (const doc of vscode.workspace.textDocuments) {
      this.open(doc);
    }
  }

  // Prefer the server's own URI string (so its exact-match staleness lookup
  // hits) and fall back to the editor's encoding before coverage has arrived.
  private wireUri(doc: vscode.TextDocument): string {
    return this.store.serverUriFor(doc.uri) ?? doc.uri.toString();
  }

  private broadcast(method: string, params: unknown): void {
    for (const client of this.clients()) {
      client.notify(method, params);
    }
  }

  private open(doc: vscode.TextDocument): void {
    if (doc.uri.scheme !== "file") {
      return;
    }
    const params: DidOpenParams = { uri: this.wireUri(doc), version: doc.version };
    this.broadcast(Method.DidOpen, params);
  }

  private change(doc: vscode.TextDocument): void {
    if (doc.uri.scheme !== "file") {
      return;
    }
    const key = doc.uri.fsPath;
    const existing = this.changeTimers.get(key);
    if (existing) {
      clearTimeout(existing);
    }
    this.changeTimers.set(
      key,
      setTimeout(() => {
        this.changeTimers.delete(key);
        const params: DidChangeParams = { uri: this.wireUri(doc), version: doc.version };
        this.broadcast(Method.DidChange, params);
      }, DocumentSync.DEBOUNCE_MS),
    );
  }

  private close(doc: vscode.TextDocument): void {
    if (doc.uri.scheme !== "file") {
      return;
    }
    // Drop a pending debounced didChange so it can't fire after didClose.
    const pending = this.changeTimers.get(doc.uri.fsPath);
    if (pending) {
      clearTimeout(pending);
      this.changeTimers.delete(doc.uri.fsPath);
    }
    const params: DidCloseParams = { uri: this.wireUri(doc) };
    this.broadcast(Method.DidClose, params);
  }

  dispose(): void {
    for (const timer of this.changeTimers.values()) {
      clearTimeout(timer);
    }
    this.changeTimers.clear();
    for (const d of this.disposables) {
      d.dispose();
    }
  }
}
