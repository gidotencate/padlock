import * as vscode from "vscode";
import * as cp from "child_process";
import * as fs from "fs";
import * as os from "os";
import * as path from "path";

// ── Types matching padlock's --json output ────────────────────────────────────

interface PadlockGap {
  after_field: string;
  bytes: number;
  at_offset: number;
}

interface PadlockFinding {
  kind: string;
  severity: "High" | "Medium" | "Low";
  struct_name: string;
  wasted_bytes?: number;
  savings?: number;
  gaps?: PadlockGap[];
  /** True when the finding was derived from type-name heuristics rather than explicit annotations. */
  is_inferred?: boolean;
}

interface PadlockStruct {
  struct_name: string;
  source_file: string;
  source_line: number;
  total_size: number;
  wasted_bytes: number;
  score: number;
  findings: PadlockFinding[];
}

interface PadlockOutput {
  structs: PadlockStruct[];
}

// ── Extension state ───────────────────────────────────────────────────────────

let diagnosticCollection: vscode.DiagnosticCollection;
let statusBarItem: vscode.StatusBarItem;
let saveDebounceTimers = new Map<string, ReturnType<typeof setTimeout>>();

/** Cached per-file analysis results: VS Code URI string → PadlockStruct[]. */
let fileStructCache = new Map<string, PadlockStruct[]>();

/** Virtual documents for fix-preview diff editor. */
const PREVIEW_SCHEME = "padlock-preview";
const previewContentMap = new Map<string, string>();

const DEBOUNCE_MS = 600;
const SUPPORTED_LANGS = ["rust", "c", "cpp", "go", "zig"];

// ── Activation ────────────────────────────────────────────────────────────────

export function activate(context: vscode.ExtensionContext): void {
  diagnosticCollection =
    vscode.languages.createDiagnosticCollection("padlock");

  // Status bar — bottom-right, click to re-analyse
  statusBarItem = vscode.window.createStatusBarItem(
    vscode.StatusBarAlignment.Right,
    100,
  );
  statusBarItem.command = "padlock.analyzeFile";
  statusBarItem.tooltip = "padlock struct layout · click to re-analyse";

  // Content provider that serves fixed-file content for the diff editor
  const contentProvider: vscode.TextDocumentContentProvider = {
    provideTextDocumentContent(uri: vscode.Uri): string {
      return previewContentMap.get(uri.toString()) ?? "";
    },
  };

  context.subscriptions.push(
    diagnosticCollection,
    statusBarItem,
    vscode.workspace.registerTextDocumentContentProvider(
      PREVIEW_SCHEME,
      contentProvider,
    ),

    // ── Commands ──────────────────────────────────────────────────────────────

    vscode.commands.registerCommand("padlock.analyzeFile", () => {
      const doc = vscode.window.activeTextEditor?.document;
      if (doc) {
        analyzeFile(doc.uri.fsPath);
      }
    }),

    vscode.commands.registerCommand("padlock.analyzeWorkspace", () => {
      analyzeWorkspace();
    }),

    // Fix all structs directly (no preview) — existing command, unchanged behaviour
    vscode.commands.registerCommand("padlock.fixFile", async () => {
      const doc = vscode.window.activeTextEditor?.document;
      if (!doc) {
        return;
      }
      const exe = resolveExecutable();
      await runCommand(exe, ["fix", doc.uri.fsPath]).catch(() => {});
      analyzeFile(doc.uri.fsPath);
    }),

    // Fix a single struct by name — used by the CodeAction quick-fix
    vscode.commands.registerCommand(
      "padlock.fixStruct",
      async (filePath: string, filter: string) => {
        const exe = resolveExecutable();
        const args = ["fix", filePath];
        if (filter) {
          args.push("--filter", filter);
        }
        await runCommand(exe, args).catch(() => {});
        analyzeFile(filePath);
      },
    ),

    // Show a diff preview of all reorder changes, then ask to apply
    vscode.commands.registerCommand("padlock.fixFilePreview", () => {
      showFixPreview();
    }),

    vscode.commands.registerCommand("padlock.clearDiagnostics", () => {
      diagnosticCollection.clear();
      fileStructCache.clear();
      updateStatusBar(vscode.window.activeTextEditor?.document);
    }),

    // ── Hover provider ────────────────────────────────────────────────────────

    vscode.languages.registerHoverProvider(SUPPORTED_LANGS, {
      provideHover(
        document: vscode.TextDocument,
        position: vscode.Position,
      ): vscode.Hover | null {
        return buildHover(document, position);
      },
    }),

    // ── CodeAction (quick-fix lightbulb) ──────────────────────────────────────

    vscode.languages.registerCodeActionsProvider(
      SUPPORTED_LANGS,
      {
        provideCodeActions(
          document: vscode.TextDocument,
          range: vscode.Range,
          context: vscode.CodeActionContext,
        ): vscode.CodeAction[] {
          return buildCodeActions(document, range, context);
        },
      },
      { providedCodeActionKinds: [vscode.CodeActionKind.QuickFix] },
    ),

    // ── Run on save ───────────────────────────────────────────────────────────

    vscode.workspace.onDidSaveTextDocument((doc) => {
      if (!isSupportedFile(doc.fileName)) {
        return;
      }
      const cfg = vscode.workspace.getConfiguration("padlock");
      if (!cfg.get<boolean>("runOnSave", true)) {
        return;
      }
      scheduleAnalysis(doc.uri.fsPath);
    }),

    // Update status bar when the active editor changes
    vscode.window.onDidChangeActiveTextEditor((editor) => {
      updateStatusBar(editor?.document);
    }),

    // Clean up per-file state when a file is closed
    vscode.workspace.onDidCloseTextDocument((doc) => {
      diagnosticCollection.delete(doc.uri);
      fileStructCache.delete(doc.uri.toString());
      updateStatusBar(vscode.window.activeTextEditor?.document);
    }),
  );

  // Initialise status bar for the already-open editor (if any)
  updateStatusBar(vscode.window.activeTextEditor?.document);
}

export function deactivate(): void {
  saveDebounceTimers.forEach((t) => clearTimeout(t));
}

// ── Analysis ──────────────────────────────────────────────────────────────────

function scheduleAnalysis(filePath: string): void {
  const existing = saveDebounceTimers.get(filePath);
  if (existing) {
    clearTimeout(existing);
  }
  const timer = setTimeout(() => {
    saveDebounceTimers.delete(filePath);
    analyzeFile(filePath);
  }, DEBOUNCE_MS);
  saveDebounceTimers.set(filePath, timer);
}

function analyzeFile(filePath: string): void {
  const exe = resolveExecutable();
  const cfg = vscode.workspace.getConfiguration("padlock");
  const extra = cfg.get<string[]>("extraArgs", []);

  // Show spinner in status bar while analysis runs
  const fileUri = vscode.Uri.file(filePath);
  if (vscode.window.activeTextEditor?.document.uri.toString() === fileUri.toString()) {
    statusBarItem.text = "$(sync~spin) padlock";
    statusBarItem.show();
  }

  runCommand(exe, ["analyze", "--json", filePath, ...extra])
    .then((output) => applyDiagnostics(output, filePath))
    .catch((err) => {
      if ((err as NodeJS.ErrnoException).code === "ENOENT") {
        vscode.window
          .showWarningMessage(
            `padlock: executable '${exe}' not found. Install with: cargo install padlock-cli`,
            "Install instructions",
          )
          .then((choice) => {
            if (choice) {
              vscode.env.openExternal(
                vscode.Uri.parse(
                  "https://github.com/gidotencate/padlock#installation",
                ),
              );
            }
          });
      }
      // Restore status bar to previous state on error
      updateStatusBar(vscode.window.activeTextEditor?.document);
    });
}

function analyzeWorkspace(): void {
  const folders = vscode.workspace.workspaceFolders;
  if (!folders || folders.length === 0) {
    vscode.window.showInformationMessage("padlock: no workspace folder open.");
    return;
  }
  const exe = resolveExecutable();
  const cfg = vscode.workspace.getConfiguration("padlock");
  const extra = cfg.get<string[]>("extraArgs", []);
  const roots = folders.map((f) => f.uri.fsPath);

  statusBarItem.text = "$(sync~spin) padlock";
  statusBarItem.show();
  diagnosticCollection.clear();

  vscode.window.withProgress(
    {
      location: vscode.ProgressLocation.Window,
      title: "padlock: analysing workspace…",
    },
    () =>
      runCommand(exe, ["analyze", "--json", ...roots, ...extra])
        .then((output) => applyDiagnostics(output))
        .catch(() => {
          updateStatusBar(vscode.window.activeTextEditor?.document);
        }),
  );
}

// ── Fix preview ───────────────────────────────────────────────────────────────

async function showFixPreview(): Promise<void> {
  const editor = vscode.window.activeTextEditor;
  if (!editor) {
    return;
  }

  const filePath = editor.document.uri.fsPath;
  const exe = resolveExecutable();
  const ext = path.extname(filePath);
  const tmpPath = path.join(
    os.tmpdir(),
    `padlock-preview-${Date.now()}${ext}`,
  );

  try {
    fs.copyFileSync(filePath, tmpPath);

    // Run fix on the temp copy; padlock creates a .bak alongside it
    try {
      await runCommand(exe, ["fix", tmpPath]);
    } catch {
      vscode.window.showInformationMessage(
        "padlock: all structs are already optimally ordered.",
      );
      return;
    }

    const fixedContent = fs.readFileSync(tmpPath, "utf8");
    const originalContent = editor.document.getText();

    if (fixedContent === originalContent) {
      vscode.window.showInformationMessage(
        "padlock: all structs are already optimally ordered.",
      );
      return;
    }

    // Register fixed content under a unique virtual URI
    const label = `${path.basename(filePath)} (padlock fix preview)`;
    const previewUri = vscode.Uri.parse(
      `${PREVIEW_SCHEME}:${encodeURIComponent(label)}?t=${Date.now()}`,
    );
    previewContentMap.set(previewUri.toString(), fixedContent);

    // Open the native diff editor: left = current file, right = preview
    await vscode.commands.executeCommand(
      "vscode.diff",
      editor.document.uri,
      previewUri,
      `${path.basename(filePath)}  ↔  padlock fix preview`,
      { preview: true },
    );

    // Ask the user whether to apply
    const choice = await vscode.window.showInformationMessage(
      "Apply padlock field reorderings to this file?",
      { modal: false },
      "Apply",
      "Dismiss",
    );

    if (choice === "Apply") {
      // Back up original, write fixed content
      const bakPath = filePath.replace(/(\.[^.]+)$/, "$1.bak");
      fs.copyFileSync(filePath, bakPath);
      fs.writeFileSync(filePath, fixedContent);
      analyzeFile(filePath);
      vscode.window.showInformationMessage(
        `padlock: rewrote ${path.basename(filePath)}. Backup: ${path.basename(bakPath)}`,
      );
    }

    // Clean up virtual document entry
    previewContentMap.delete(previewUri.toString());
  } finally {
    // Remove temp file and the .bak padlock created next to it
    try {
      fs.unlinkSync(tmpPath);
    } catch {}
    try {
      fs.unlinkSync(tmpPath + ".bak");
    } catch {}
  }
}

// ── Hover provider ────────────────────────────────────────────────────────────

function buildHover(
  document: vscode.TextDocument,
  position: vscode.Position,
): vscode.Hover | null {
  const structs = fileStructCache.get(document.uri.toString());
  if (!structs) {
    return null;
  }

  // source_line is 1-based; VS Code position.line is 0-based
  const line = position.line + 1;
  const struct = structs.find((s) => s.source_line === line);
  if (!struct || struct.findings.length === 0) {
    return null;
  }

  const md = new vscode.MarkdownString("", true);
  md.isTrusted = true;
  md.supportHtml = false;

  // Header with struct name
  md.appendMarkdown(`**padlock** — \`${struct.struct_name}\`\n\n`);

  // Score bar (10 blocks)
  const filled = Math.round(struct.score / 10);
  const scoreBar =
    "█".repeat(filled) + "░".repeat(10 - filled);
  md.appendMarkdown(
    `Score **${struct.score}**/100 \`${scoreBar}\` · ${struct.total_size}B`,
  );
  if (struct.wasted_bytes > 0) {
    md.appendMarkdown(` · **${struct.wasted_bytes}B wasted**`);
  }
  md.appendMarkdown("\n\n");

  // Findings
  for (const f of struct.findings) {
    const icon =
      f.severity === "High" ? "🔴" : f.severity === "Medium" ? "🟡" : "🔵";
    md.appendMarkdown(
      `${icon} **${f.kind}** — ${formatFindingBrief(f, struct)}\n\n`,
    );
  }

  return new vscode.Hover(md);
}

function formatFindingBrief(f: PadlockFinding, s: PadlockStruct): string {
  switch (f.kind) {
    case "PaddingWaste": {
      const pct =
        s.total_size > 0
          ? Math.round((s.wasted_bytes / s.total_size) * 100)
          : 0;
      return `${s.wasted_bytes}B wasted (${pct}% of ${s.total_size}B)`;
    }
    case "ReorderSuggestion":
      return `reorder saves ${f.savings ?? 0}B → ${s.total_size - (f.savings ?? 0)}B total`;
    case "FalseSharing": {
      const inferred = f.is_inferred ? " _(inferred — verify with profiling or add guard annotations)_" : "";
      return `concurrent fields share a cache line${inferred}`;
    }
    case "LocalityIssue": {
      const inferred = f.is_inferred ? " _(inferred — verify with profiling)_" : "";
      return `hot/cold fields interleaved — group hot fields first${inferred}`;
    }
    default:
      return f.kind;
  }
}

// ── CodeAction provider ───────────────────────────────────────────────────────

function buildCodeActions(
  document: vscode.TextDocument,
  range: vscode.Range,
  context: vscode.CodeActionContext,
): vscode.CodeAction[] {
  const actions: vscode.CodeAction[] = [];

  const reorderDiags = context.diagnostics.filter(
    (d) => d.code === "ReorderSuggestion",
  );
  if (reorderDiags.length > 0) {
    const structs = fileStructCache.get(document.uri.toString()) ?? [];
    const line = range.start.line + 1; // 1-based
    const struct = structs.find((s) => s.source_line === line);

    if (struct) {
      // Quick-fix for this specific struct
      const fixOne = new vscode.CodeAction(
        `Reorder \`${struct.struct_name}\` fields (padlock)`,
        vscode.CodeActionKind.QuickFix,
      );
      fixOne.command = {
        command: "padlock.fixStruct",
        title: `Reorder ${struct.struct_name}`,
        arguments: [
          document.uri.fsPath,
          `^${escapeRegex(struct.struct_name)}$`,
        ],
      };
      fixOne.diagnostics = reorderDiags;
      fixOne.isPreferred = true;
      actions.push(fixOne);
    }
  }

  // "Fix all → preview" whenever the file has any reorder diagnostic
  const allDiags = diagnosticCollection.get(document.uri) ?? [];
  const fileHasReorders = allDiags.some((d) => d.code === "ReorderSuggestion");
  if (fileHasReorders) {
    const fixAll = new vscode.CodeAction(
      "Fix all reorder suggestions in file — preview (padlock)",
      vscode.CodeActionKind.QuickFix,
    );
    fixAll.command = {
      command: "padlock.fixFilePreview",
      title: "Fix all reorder suggestions — preview",
    };
    actions.push(fixAll);
  }

  return actions;
}

// ── Status bar ────────────────────────────────────────────────────────────────

function updateStatusBar(doc: vscode.TextDocument | undefined): void {
  if (!doc || !isSupportedFile(doc.fileName)) {
    statusBarItem.hide();
    return;
  }

  const structs = fileStructCache.get(doc.uri.toString());

  if (!structs) {
    // File not yet analysed — show neutral state
    statusBarItem.text = "$(lock) padlock";
    statusBarItem.backgroundColor = undefined;
    statusBarItem.show();
    return;
  }

  if (structs.length === 0) {
    statusBarItem.text = "$(lock) padlock $(check)";
    statusBarItem.tooltip = "padlock: no structs found in this file";
    statusBarItem.backgroundColor = undefined;
    statusBarItem.show();
    return;
  }

  // Weighted aggregate score (same formula as `padlock summary`)
  const totalWeight = structs.reduce((sum, s) => sum + s.total_size, 0);
  const score =
    totalWeight > 0
      ? Math.round(
          structs.reduce((sum, s) => sum + s.score * s.total_size, 0) /
            totalWeight,
        )
      : 100;

  const grade =
    score >= 90
      ? "A"
      : score >= 80
        ? "B"
        : score >= 70
          ? "C"
          : score >= 60
            ? "D"
            : "F";

  const allFindings = structs.flatMap((s) => s.findings);
  const highCount = allFindings.filter((f) => f.severity === "High").length;
  const medCount = allFindings.filter((f) => f.severity === "Medium").length;

  let text = `$(lock) ${score} ${grade}`;
  let tooltip = `padlock — score: ${score}/100 (${grade})\n`;

  if (highCount > 0) {
    text += `  $(warning) ${highCount}`;
    tooltip += `${highCount} High · ${medCount} Medium findings`;
    statusBarItem.backgroundColor = new vscode.ThemeColor(
      "statusBarItem.warningBackground",
    );
  } else if (medCount > 0) {
    text += `  $(info) ${medCount}`;
    tooltip += `${medCount} Medium findings`;
    statusBarItem.backgroundColor = undefined;
  } else {
    tooltip += "No findings";
    statusBarItem.backgroundColor = undefined;
  }

  tooltip += "\n\nClick to re-analyse";
  statusBarItem.text = text;
  statusBarItem.tooltip = tooltip;
  statusBarItem.show();
}

// ── Diagnostic conversion ─────────────────────────────────────────────────────

function applyDiagnostics(jsonOutput: string, scopeFile?: string): void {
  let parsed: PadlockOutput;
  try {
    parsed = JSON.parse(jsonOutput) as PadlockOutput;
  } catch {
    return; // empty output or non-JSON (no structs found)
  }

  const cfg = vscode.workspace.getConfiguration("padlock");
  const minSeverity = cfg.get<string>("severity", "high");

  // Group structs by resolved file URI
  const structsByUri = new Map<string, PadlockStruct[]>();
  for (const s of parsed.structs) {
    if (!s.source_file || !s.source_line) {
      continue;
    }
    const filePath = path.isAbsolute(s.source_file)
      ? s.source_file
      : path.join(
          vscode.workspace.workspaceFolders?.[0]?.uri.fsPath ?? "",
          s.source_file,
        );
    const key = vscode.Uri.file(filePath).toString();
    if (!structsByUri.has(key)) {
      structsByUri.set(key, []);
    }
    structsByUri.get(key)!.push(s);
  }

  // Update struct cache (used by hover and CodeAction providers)
  if (scopeFile) {
    const scopeKey = vscode.Uri.file(scopeFile).toString();
    fileStructCache.set(scopeKey, structsByUri.get(scopeKey) ?? []);
  } else {
    fileStructCache.clear();
    structsByUri.forEach((structs, key) => fileStructCache.set(key, structs));
  }

  // Build and apply diagnostics
  const byUri = new Map<string, vscode.Diagnostic[]>();
  for (const [uriKey, structs] of structsByUri) {
    const diags: vscode.Diagnostic[] = [];
    for (const s of structs) {
      for (const finding of s.findings) {
        if (!shouldShow(finding.severity, minSeverity)) {
          continue;
        }
        diags.push(makeDiagnostic(s, finding));
      }
    }
    byUri.set(uriKey, diags);
  }

  if (scopeFile) {
    const scopeUri = vscode.Uri.file(scopeFile);
    diagnosticCollection.set(
      scopeUri,
      byUri.get(scopeUri.toString()) ?? [],
    );
  } else {
    diagnosticCollection.clear();
    byUri.forEach((diags, key) => {
      diagnosticCollection.set(vscode.Uri.parse(key), diags);
    });
  }

  // Refresh status bar with new data
  updateStatusBar(vscode.window.activeTextEditor?.document);
}

function makeDiagnostic(
  s: PadlockStruct,
  finding: PadlockFinding,
): vscode.Diagnostic {
  const line = Math.max(0, s.source_line - 1);
  const range = new vscode.Range(line, 0, line, Number.MAX_SAFE_INTEGER);
  const message = formatMessage(s, finding);
  const severity = mapSeverity(finding.severity);

  const diag = new vscode.Diagnostic(range, message, severity);
  diag.source = "padlock";
  diag.code = finding.kind;
  return diag;
}

function formatMessage(s: PadlockStruct, f: PadlockFinding): string {
  switch (f.kind) {
    case "PaddingWaste": {
      const pct =
        s.total_size > 0
          ? Math.round((s.wasted_bytes / s.total_size) * 100)
          : 0;
      return (
        `${s.struct_name}: ${s.wasted_bytes}B wasted (${pct}% of ${s.total_size}B). ` +
        `Run 'padlock explain ${s.source_file}' for the full layout.`
      );
    }
    case "ReorderSuggestion":
      return (
        `${s.struct_name}: reordering fields saves ${f.savings ?? 0}B ` +
        `(${s.total_size}B → ${s.total_size - (f.savings ?? 0)}B). ` +
        `Use 'padlock: Apply fix' or the lightbulb (⚡) to reorder.`
      );
    case "FalseSharing": {
      const inferred = f.is_inferred
        ? " (inferred from type names — add guard annotations or verify with profiling)"
        : "";
      return (
        `${s.struct_name}: false sharing — fields guarded by different locks ` +
        `share a cache line. Consider separating with #[repr(align(64))].${inferred}`
      );
    }
    case "LocalityIssue": {
      const inferred = f.is_inferred
        ? " (inferred from type names — verify with profiling)"
        : "";
      return (
        `${s.struct_name}: hot and cold fields are interleaved — ` +
        `group frequently-accessed fields at the start of the struct.${inferred}`
      );
    }
    default:
      return `${s.struct_name}: ${f.kind} (score ${s.score})`;
  }
}

function mapSeverity(
  s: "High" | "Medium" | "Low",
): vscode.DiagnosticSeverity {
  switch (s) {
    case "High":
      return vscode.DiagnosticSeverity.Warning;
    case "Medium":
      return vscode.DiagnosticSeverity.Information;
    case "Low":
      return vscode.DiagnosticSeverity.Hint;
  }
}

function shouldShow(findingSeverity: string, minSeverity: string): boolean {
  const rank: Record<string, number> = { high: 3, medium: 2, low: 1 };
  return (
    (rank[findingSeverity.toLowerCase()] ?? 0) >= (rank[minSeverity] ?? 1)
  );
}

// ── Helpers ───────────────────────────────────────────────────────────────────

function isSupportedFile(filePath: string): boolean {
  return /\.(rs|c|cpp|cc|cxx|h|hpp|go|zig)$/.test(filePath);
}

function resolveExecutable(): string {
  return (
    vscode.workspace
      .getConfiguration("padlock")
      .get<string>("executable", "padlock") || "padlock"
  );
}

function escapeRegex(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function runCommand(exe: string, args: string[]): Promise<string> {
  return new Promise((resolve, reject) => {
    let stdout = "";
    let stderr = "";

    const proc = cp.spawn(exe, args, {
      cwd: vscode.workspace.workspaceFolders?.[0]?.uri.fsPath,
      shell: false,
    });

    proc.stdout.on("data", (chunk: Buffer) => {
      stdout += chunk.toString();
    });
    proc.stderr.on("data", (chunk: Buffer) => {
      stderr += chunk.toString();
    });

    proc.on("error", reject);

    proc.on("close", (code) => {
      // padlock exits non-zero when findings exceed a threshold — expected.
      // Only reject when there is genuinely no JSON output (real error).
      if (code !== 0 && stdout.trim() === "") {
        reject(new Error(stderr || `padlock exited with code ${code}`));
      } else {
        resolve(stdout);
      }
    });
  });
}
