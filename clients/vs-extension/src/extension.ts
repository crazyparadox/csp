import * as vscode from "vscode";

import { CspClient } from "./client";
import { CoverageStore } from "./coverageStore";
import { DecorationManager } from "./decorations";
import { DocumentSync } from "./docSync";
import { Method } from "./protocol";
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
    void vscode.commands.executeCommand("setContext", "csp.active", clients.length > 0);
    decorations.repaintAll();
  };

  const stopAll = async () => {
    const toStop = clients;
    clients = [];
    void vscode.commands.executeCommand("setContext", "csp.active", false);
    for (const client of toStop) {
      client.dispose();
    }
  };

  // Ask every server to re-collect coverage now (the server treats csp/run as a
  // forced re-run; older report-only servers ignore the unknown notification).
  const refresh = () => {
    if (clients.length === 0) {
      void startAll();
      return;
    }
    for (const client of clients) {
      client.notify(Method.Run, {});
    }
  };

  const showMenu = async () => {
    const pick = await vscode.window.showQuickPick(
      [
        { label: "$(refresh) Refresh Coverage", command: "csp.refresh" },
        { label: "$(eye) Toggle Coverage Gutters", command: "csp.toggleGutters" },
        { label: "$(debug-restart) Restart Coverage Server", command: "csp.restart" },
        { label: "$(output) Show Server Log", command: "csp.showLog" },
      ],
      { title: "CSP Coverage", placeHolder: "Choose an action" },
    );
    if (pick) {
      void vscode.commands.executeCommand(pick.command);
    }
  };

  context.subscriptions.push(
    { dispose: () => void stopAll() },
    vscode.commands.registerCommand("csp.refresh", refresh),
    vscode.commands.registerCommand("csp.restart", () => startAll()),
    vscode.commands.registerCommand("csp.toggleGutters", () => decorations.toggle()),
    vscode.commands.registerCommand("csp.showLog", () => log.show()),
    vscode.commands.registerCommand("csp.showMenu", showMenu),
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
