import * as vscode from "vscode";

import { CoverageStore } from "./coverageStore";
import { RunStateChangedParams } from "./protocol";

// Workspace coverage indicator in the status bar: a percentage that is colored
// by coverage level, a spinner while a run is in progress, and a click target
// that opens the CSP action menu.
export class StatusBar implements vscode.Disposable {
  private readonly item: vscode.StatusBarItem;

  constructor(private readonly store: CoverageStore) {
    this.item = vscode.window.createStatusBarItem(
      "csp.coverage",
      vscode.StatusBarAlignment.Left,
      100,
    );
    this.item.name = "CSP Coverage";
    this.item.command = "csp.showMenu";
    this.render();
    this.item.show();
  }

  onRunState(params: RunStateChangedParams): void {
    if (params.state === "running") {
      this.item.text = "$(sync~spin) Coverage";
      this.item.tooltip = "CSP: collecting coverage…";
      this.item.backgroundColor = undefined;
    } else if (params.state === "errored") {
      this.item.text = "$(error) Coverage";
      this.item.tooltip = this.tooltip("Coverage run failed — open the log for details.");
      this.item.backgroundColor = new vscode.ThemeColor("statusBarItem.errorBackground");
    } else {
      this.render();
    }
    this.item.show();
  }

  private render(): void {
    const summary = this.store.getWorkspaceSummary();
    if (!summary || summary.linesTotal === 0) {
      this.item.text = "$(beaker) Coverage";
      this.item.tooltip = this.tooltip("No coverage yet.");
      this.item.backgroundColor = undefined;
      return;
    }
    const pct = (summary.linesCovered / summary.linesTotal) * 100;
    const icon = pct >= 80 ? "$(pass-filled)" : pct >= 50 ? "$(beaker)" : "$(warning)";
    this.item.text = `${icon} ${pct.toFixed(0)}%`;
    this.item.backgroundColor =
      pct < 50 ? new vscode.ThemeColor("statusBarItem.warningBackground") : undefined;
    this.item.tooltip = this.tooltip(
      `**${pct.toFixed(1)}%** — ${summary.linesCovered} / ${summary.linesTotal} lines covered`,
    );
  }

  private tooltip(headline: string): vscode.MarkdownString {
    const md = new vscode.MarkdownString(
      `$(beaker) **CSP Coverage**\n\n${headline}\n\n` +
        `[$(refresh) Refresh](command:csp.refresh) · ` +
        `[$(eye) Toggle gutters](command:csp.toggleGutters) · ` +
        `[$(output) Log](command:csp.showLog)`,
      true,
    );
    md.isTrusted = true;
    return md;
  }

  dispose(): void {
    this.item.dispose();
  }
}
