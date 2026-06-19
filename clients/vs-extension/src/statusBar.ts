import * as vscode from "vscode";

import { CoverageStore } from "./coverageStore";
import { RunStateChangedParams } from "./protocol";

// Shows workspace coverage percent in the status bar, with a spinner while a
// run is in progress.
export class StatusBar implements vscode.Disposable {
  private readonly item: vscode.StatusBarItem;

  constructor(private readonly store: CoverageStore) {
    this.item = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 100);
    this.item.command = "csp.showLog";
    this.render();
    this.item.show();
  }

  onRunState(params: RunStateChangedParams): void {
    if (params.state === "running") {
      this.item.text = "$(sync~spin) Coverage";
      this.item.tooltip = "CSP: collecting coverage…";
    } else if (params.state === "errored") {
      this.item.text = "$(error) Coverage";
      this.item.tooltip = "CSP: coverage run failed (click for log)";
    } else {
      this.render();
    }
    this.item.show();
  }

  private render(): void {
    const summary = this.store.getWorkspaceSummary();
    if (!summary || summary.linesTotal === 0) {
      this.item.text = "$(beaker) Coverage";
      this.item.tooltip = "CSP: no coverage yet";
      return;
    }
    const pct = (summary.linesCovered / summary.linesTotal) * 100;
    this.item.text = `$(beaker) ${pct.toFixed(0)}%`;
    this.item.tooltip = `CSP: ${summary.linesCovered}/${summary.linesTotal} lines covered`;
  }

  dispose(): void {
    this.item.dispose();
  }
}
