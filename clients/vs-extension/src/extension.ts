import * as vscode from "vscode";

import { CspClient } from "./client";
import { CoverageStore } from "./coverageStore";
import { DecorationManager } from "./decorations";
import { DocumentSync } from "./docSync";
import { StatusBar } from "./statusBar";

let clients: CspClient[] = [];

export function activate(context: vscode.ExtensionContext): void {
  const log = vscode.window.createOutputChannel("CSP Coverage");
  const store = new CoverageStore();
  const diagnostics = vscode.languages.createDiagnosticCollection("csp");
  const decorations = new DecorationManager(context, store);
  const statusBar = new StatusBar(store);
  const docSync = new DocumentSync(() => clients, store);

  context.subscriptions.push(log, store, diagnostics, decorations, statusBar, docSync);

  // Serialize all start/stop work behind a single promise chain so overlapping
  // invocations (initial start, csp.restart, workspace-folder changes) can't
  // interleave and orphan child processes.
  let pending: Promise<void> = Promise.resolve();
  const startAll = (): Promise<void> => {
    pending = pending.then(doStartAll, doStartAll);
    return pending;
  };

  const doStartAll = async () => {
    await stopAll();
    diagnostics.clear();
    store.clear();
    const folders = vscode.workspace.workspaceFolders ?? [];
    clients = folders.map(
      (folder) =>
        new CspClient(context, folder, store, diagnostics, log, (params) =>
          statusBar.onRunState(params),
        ),
    );
    await Promise.all(clients.map((c) => c.start()));
    decorations.repaintAll();
  };

  const stopAll = async () => {
    const toStop = clients;
    clients = [];
    for (const client of toStop) {
      client.dispose();
    }
  };

  context.subscriptions.push(
    { dispose: () => void stopAll() },
    vscode.commands.registerCommand("csp.restart", () => startAll()),
    vscode.commands.registerCommand("csp.toggleGutters", () => decorations.toggle()),
    vscode.commands.registerCommand("csp.showLog", () => log.show()),
    vscode.workspace.onDidChangeWorkspaceFolders(() => startAll()),
  );

  void startAll();
}

export function deactivate(): void {
  for (const client of clients) {
    client.dispose();
  }
  clients = [];
}
