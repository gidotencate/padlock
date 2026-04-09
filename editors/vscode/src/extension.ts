import * as vscode from "vscode";
import * as cp from "child_process";
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
let saveDebounceTimers = new Map<string, ReturnType<typeof setTimeout>>();
const DEBOUNCE_MS = 600;

// ── Activation ────────────────────────────────────────────────────────────────

export function activate(context: vscode.ExtensionContext): void {
  diagnosticCollection =
    vscode.languages.createDiagnosticCollection("padlock");
  context.subscriptions.push(diagnosticCollection);

  // Commands
  context.subscriptions.push(
    vscode.commands.registerCommand("padlock.analyzeFile", () => {
      const doc = vscode.window.activeTextEditor?.document;
      if (doc) {
        analyzeFile(doc.uri.fsPath);
      }
    }),

    vscode.commands.registerCommand("padlock.analyzeWorkspace", () => {
      analyzeWorkspace();
    }),

    vscode.commands.registerCommand("padlock.fixFile", async () => {
      const doc = vscode.window.activeTextEditor?.document;
      if (!doc) {
        return;
      }
      const filePath = doc.uri.fsPath;
      const exe = resolveExecutable();
      runCommand(exe, ["fix", filePath]).then(() => {
        // padlock rewrites the file in place; VS Code detects the change
        // automatically. Re-analyze to refresh diagnostics.
        analyzeFile(filePath);
      });
    }),

    vscode.commands.registerCommand("padlock.clearDiagnostics", () => {
      diagnosticCollection.clear();
    }),
  );

  // Run on save
  context.subscriptions.push(
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
  );

  // Clear diagnostics when a file is closed
  context.subscriptions.push(
    vscode.workspace.onDidCloseTextDocument((doc) => {
      diagnosticCollection.delete(doc.uri);
    }),
  );
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

  runCommand(exe, ["analyze", "--json", filePath, ...extra])
    .then((output) => applyDiagnostics(output, filePath))
    .catch((err) => {
      // Only show a message if the executable is simply missing
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
    });
}

function analyzeWorkspace(): void {
  const folders = vscode.workspace.workspaceFolders;
  if (!folders || folders.length === 0) {
    vscode.window.showInformationMessage(
      "padlock: no workspace folder open.",
    );
    return;
  }
  const exe = resolveExecutable();
  const cfg = vscode.workspace.getConfiguration("padlock");
  const extra = cfg.get<string[]>("extraArgs", []);
  const roots = folders.map((f) => f.uri.fsPath);

  diagnosticCollection.clear();
  vscode.window.withProgress(
    {
      location: vscode.ProgressLocation.Window,
      title: "padlock: analyzing workspace…",
    },
    () =>
      runCommand(exe, ["analyze", "--json", ...roots, ...extra])
        .then((output) => applyDiagnostics(output))
        .catch(() => {
          /* errors already surfaced in analyzeFile */
        }),
  );
}

// ── Diagnostic conversion ─────────────────────────────────────────────────────

function applyDiagnostics(
  jsonOutput: string,
  scopeFile?: string,
): void {
  let parsed: PadlockOutput;
  try {
    parsed = JSON.parse(jsonOutput) as PadlockOutput;
  } catch {
    return; // empty output or non-JSON (no structs found)
  }

  const cfg = vscode.workspace.getConfiguration("padlock");
  const minSeverity = cfg.get<string>("severity", "high");

  // Group diagnostics by file
  const byFile = new Map<string, vscode.Diagnostic[]>();

  for (const s of parsed.structs) {
    if (!s.source_file || !s.source_line) {
      continue;
    }

    // Resolve path relative to workspace root when it's not absolute
    const filePath = path.isAbsolute(s.source_file)
      ? s.source_file
      : path.join(
          vscode.workspace.workspaceFolders?.[0]?.uri.fsPath ?? "",
          s.source_file,
        );

    const fileUri = vscode.Uri.file(filePath);
    const key = fileUri.toString();

    if (!byFile.has(key)) {
      byFile.set(key, []);
    }

    for (const finding of s.findings) {
      if (!shouldShow(finding.severity, minSeverity)) {
        continue;
      }

      const diag = makeDiagnostic(s, finding);
      byFile.get(key)!.push(diag);
    }
  }

  // If scoped to one file, only update that file's diagnostics
  if (scopeFile) {
    const scopeUri = vscode.Uri.file(scopeFile);
    const key = scopeUri.toString();
    diagnosticCollection.set(scopeUri, byFile.get(key) ?? []);
  } else {
    // Full workspace update: clear old and set all new
    diagnosticCollection.clear();
    byFile.forEach((diags, key) => {
      diagnosticCollection.set(vscode.Uri.parse(key), diags);
    });
  }
}

function makeDiagnostic(
  s: PadlockStruct,
  finding: PadlockFinding,
): vscode.Diagnostic {
  // Point at the struct definition line (1-based → 0-based)
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
      const pct = s.total_size > 0
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
        `Use 'padlock: Apply fix' to reorder automatically.`
      );
    case "FalseSharing":
      return (
        `${s.struct_name}: false sharing — fields guarded by different locks ` +
        `share a cache line. Consider separating with #[repr(align(64))].`
      );
    case "LocalityIssue":
      return (
        `${s.struct_name}: hot and cold fields are interleaved — ` +
        `group frequently-accessed fields at the start of the struct.`
      );
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

function shouldShow(
  findingSeverity: string,
  minSeverity: string,
): boolean {
  const rank: Record<string, number> = { high: 3, medium: 2, low: 1 };
  return (rank[findingSeverity.toLowerCase()] ?? 0) >= (rank[minSeverity] ?? 1);
}

// ── Helpers ───────────────────────────────────────────────────────────────────

function isSupportedFile(filePath: string): boolean {
  return /\.(rs|c|cpp|cc|cxx|h|hpp|go)$/.test(filePath);
}

function resolveExecutable(): string {
  return (
    vscode.workspace
      .getConfiguration("padlock")
      .get<string>("executable", "padlock") || "padlock"
  );
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
      // padlock exits non-zero when findings exceed the threshold — that's
      // expected. Only reject if we got no JSON at all (real error).
      if (code !== 0 && stdout.trim() === "") {
        reject(new Error(stderr || `padlock exited with code ${code}`));
      } else {
        resolve(stdout);
      }
    });
  });
}
