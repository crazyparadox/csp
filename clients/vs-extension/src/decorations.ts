import * as vscode from "vscode";

import { CoverageStore } from "./coverageStore";

// Paints covered / uncovered / stale lines in the gutter (and overview ruler)
// of visible editors from the CoverageStore.
export class DecorationManager implements vscode.Disposable {
  private readonly covered: vscode.TextEditorDecorationType;
  private readonly uncovered: vscode.TextEditorDecorationType;
  private readonly stale: vscode.TextEditorDecorationType;
  private readonly disposables: vscode.Disposable[] = [];
  private enabled: boolean;

  constructor(
    context: vscode.ExtensionContext,
    private readonly store: CoverageStore,
  ) {
    const icon = (name: string) =>
      vscode.Uri.joinPath(context.extensionUri, "media", name);

    this.covered = vscode.window.createTextEditorDecorationType({
      gutterIconPath: icon("covered.svg"),
      gutterIconSize: "contain",
      overviewRulerColor: "#3fb95080",
      overviewRulerLane: vscode.OverviewRulerLane.Left,
    });
    this.uncovered = vscode.window.createTextEditorDecorationType({
      gutterIconPath: icon("uncovered.svg"),
      gutterIconSize: "contain",
      overviewRulerColor: "#f8514980",
      overviewRulerLane: vscode.OverviewRulerLane.Left,
    });
    this.stale = vscode.window.createTextEditorDecorationType({
      gutterIconPath: icon("stale.svg"),
      gutterIconSize: "contain",
    });

    this.enabled = vscode.workspace
      .getConfiguration("csp")
      .get<boolean>("enableGutters", true);

    this.disposables.push(
      this.store.onDidChange((fsPath) => this.repaintPath(fsPath)),
      vscode.window.onDidChangeVisibleTextEditors(() => this.repaintAll()),
      vscode.workspace.onDidChangeConfiguration((e) => {
        if (e.affectsConfiguration("csp.enableGutters")) {
          this.enabled = vscode.workspace
            .getConfiguration("csp")
            .get<boolean>("enableGutters", true);
          this.repaintAll();
        }
      }),
    );
  }

  toggle(): void {
    this.enabled = !this.enabled;
    void vscode.workspace
      .getConfiguration("csp")
      .update("enableGutters", this.enabled, vscode.ConfigurationTarget.Workspace);
    this.repaintAll();
  }

  repaintAll(): void {
    for (const editor of vscode.window.visibleTextEditors) {
      this.repaint(editor);
    }
  }

  private repaintPath(fsPath: string): void {
    for (const editor of vscode.window.visibleTextEditors) {
      if (editor.document.uri.fsPath === fsPath) {
        this.repaint(editor);
      }
    }
  }

  private repaint(editor: vscode.TextEditor): void {
    if (!this.enabled) {
      this.clear(editor);
      return;
    }
    const coverage = this.store.get(editor.document.uri);
    if (!coverage) {
      this.clear(editor);
      return;
    }

    const lineCount = editor.document.lineCount;
    const coveredRanges: vscode.Range[] = [];
    const uncoveredRanges: vscode.Range[] = [];
    const staleRanges: vscode.Range[] = [];

    for (const { line, hits } of coverage.lines) {
      // CSP line numbers are 0-based (LSP-style), matching the editor model.
      if (line < 0 || line >= lineCount) {
        continue;
      }
      const range = new vscode.Range(line, 0, line, 0);
      if (coverage.stale) {
        staleRanges.push(range);
      } else if (hits > 0) {
        coveredRanges.push(range);
      } else {
        uncoveredRanges.push(range);
      }
    }

    editor.setDecorations(this.covered, coveredRanges);
    editor.setDecorations(this.uncovered, uncoveredRanges);
    editor.setDecorations(this.stale, staleRanges);
  }

  private clear(editor: vscode.TextEditor): void {
    editor.setDecorations(this.covered, []);
    editor.setDecorations(this.uncovered, []);
    editor.setDecorations(this.stale, []);
  }

  dispose(): void {
    for (const d of this.disposables) {
      d.dispose();
    }
    this.covered.dispose();
    this.uncovered.dispose();
    this.stale.dispose();
  }
}
