# padlock for VS Code

**The lint pass for struct memory layout** — finds padding waste, false sharing, and cache locality problems in C, C++, Rust, and Go, shown directly in the Problems panel.

## What it does

When you save a supported file, padlock runs in the background and populates the Problems panel with layout findings. No squiggles, no interruptions — just findings waiting when you want them.

```
WARNING  Connection: 10B wasted (41%). Run 'padlock explain' for full layout.   connection.rs  line 4
WARNING  Connection: reordering fields saves 8B (24B → 16B). Use 'padlock: Apply fix'.   connection.rs  line 4
```

## Requirements

padlock must be installed and on your `PATH`:

```bash
cargo install padlock-cli
```

## Commands

| Command | Description |
|---|---|
| `padlock: Analyze current file` | Run analysis on the open file |
| `padlock: Analyze workspace` | Run analysis across the entire workspace |
| `padlock: Apply fix (reorder fields)` | Reorder fields in the current file to the optimal layout |
| `padlock: Clear findings` | Remove all padlock diagnostics from the Problems panel |

## Settings

| Setting | Default | Description |
|---|---|---|
| `padlock.runOnSave` | `true` | Analyze automatically on file save |
| `padlock.severity` | `"high"` | Minimum severity: `high`, `medium`, or `low` |
| `padlock.executable` | `"padlock"` | Path to the padlock binary |
| `padlock.extraArgs` | `[]` | Extra arguments passed to every analysis run |

### Example: only show High findings, use a custom binary path

```json
{
  "padlock.severity": "high",
  "padlock.executable": "/home/user/.cargo/bin/padlock",
  "padlock.extraArgs": ["--min-size", "16"]
}
```

## Severity mapping

padlock findings appear in the Problems panel with the following VS Code severity levels:

| padlock severity | VS Code level | Icon |
|---|---|---|
| High | Warning | ⚠ yellow |
| Medium | Information | ℹ blue |
| Low | Hint | subtle underline |

Layout issues are never shown as errors — they don't prevent your code from compiling.

## How it fits with other tools

- **CI**: use the [padlock GitHub Action](https://github.com/gidotencate/padlock) to block PRs on High findings
- **Pre-commit**: add padlock to your git hooks to catch issues before they reach the repo
- **Runtime profiling**: use padlock findings as a guide for where to focus with `perf c2c` or VTune
