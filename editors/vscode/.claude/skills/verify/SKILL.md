---
name: verify
description: Build, install, and confirm the padlock VS Code extension actually analyzes files in a real running VS Code window — not just that it compiles or type-checks.
---

# Verifying the padlock VS Code extension

The surface is a running VS Code window analyzing a real file on save. Compiling or type-checking proves nothing about that — this extension has broken silently in production-equivalent ways that `tsc` was clean through (2026-10-10, twice).

## Build and install

```bash
cd editors/vscode
npm run compile                 # tsc -p ./
rm -f padlock-*.vsix
npx vsce package
code --install-extension padlock-X.Y.Z.vsix --force
```

`code` here must be the **remote-cli** binary for the window you're testing against (find it under `~/.vscode-server/bin/*/bin/remote-cli/code` in a WSL/remote session), not a plain `code` on PATH — otherwise the install silently targets the wrong VS Code instance.

## Critical gotcha #1: the resolved `padlock` binary is a separate install from this repo's build

The extension's default `padlock.executable` setting is just `"padlock"`, resolved via the extension host's PATH — typically `~/.cargo/bin/padlock` from a prior `cargo install`, **not** `target/release/padlock`. Check it explicitly:

```bash
which padlock && padlock --version
```

If it's stale, update it from the repo root:

```bash
cargo install --path crates/padlock-cli --force
```

Root-cause incident (2026-10-10): the global binary was `0.12.0` while the extension and source were at `0.17.0`. The extension's on-save call passes `--no-cache` (added in `0.14.0`); the stale binary rejected it with a clap usage error on every single save, and `analyzeFile()`'s catch handler only showed a warning for `ENOENT` — so this failed **completely silently** for the rest of the session. That silent-swallow bug is now fixed (any non-`ENOENT` failure shows a warning), but the stale-binary trap itself is a standing hazard on any dev machine — check it first whenever "the extension doesn't analyze" is reported.

## Critical gotcha #2: a reinstalled extension needs a window reload

`code --install-extension --force` updates the files on disk but does **not** hot-swap a currently-running extension host. Any VS Code window already open keeps running the old `extension.js` in memory until it's reloaded. Check which version is actually active:

```bash
code --list-extensions --show-versions | grep padlock   # what's installed on disk
```
...then cross-check against the live extension host logs (WSL/remote):
```bash
grep -i padlock ~/.vscode-server/data/logs/*/exthost*/remoteexthost.log | tail
```
Each line's timestamp vs. the install time tells you whether that window's extension host started before or after your rebuild. If before, that window needs **Developer: Reload Window** (or closing and reopening it) before your change takes effect there — there is no way to force this from a coding-agent session; ask the person to do it.

## Driving an actual save event

There is currently **no tool in this environment that can type into or save a file in the live VS Code editor** (only `mcp__claude-vscode__getDiagnostics`, which reads state, is available — no execute-command or send-keystroke tool). This means the final step — "open a C/C++/Rust/Go/Zig file, make an edit, save it, confirm diagnostics appear" — cannot be done unattended from here. After rebuilding/reinstalling:

1. Tell the person which window(s) need a reload (gotcha #2).
2. Ask them to open a file with a wasteful struct (or edit one) and save it.
3. Use `mcp__claude-vscode__getDiagnostics` (optionally with `uri` for that one file) to confirm a `"source": "padlock"` entry appears.

A clean diagnostics result is ambiguous on its own — padlock only emits diagnostics for structs with findings, so "no padlock diagnostics" can mean either "broken" or "genuinely no padding waste in this file." Use a file you know has waste (e.g. `struct { char a; int b; char c; };`) to remove that ambiguity.

## If this gap gets closed later

If a future tool addition lets a session drive the real editor (open a file, edit it, save it) or run `@vscode/test-electron` headless (needs `xvfb` + the test-electron package, neither present as of 2026-10-10), update this skill to drive the save directly instead of handing it to the person.
