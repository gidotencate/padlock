---
name: verify
description: Build and run the padlock CLI to confirm a code change works, for the Rust workspace (crates/padlock-*). For the VS Code extension, see editors/vscode/.claude/skills/verify/SKILL.md instead.
---

# Verifying padlock-cli changes

The surface is the `padlock` binary itself — never `cargo test` alone as the verification. Run tests for regression coverage, then drive the actual CLI.

## Build

```bash
cargo build --release    # or just the touched crate: cargo build --release -p padlock-cli
```

## Drive it

Most subcommands need real source files or a real git repo, not mocks:

```bash
./target/release/padlock analyze --json path/to/file.c
./target/release/padlock --version
```

### `diff --against <git-ref>` needs a throwaway git repo

This command's whole job is comparing struct layout across git history, so build a real one instead of trusting unit tests:

```bash
TMP=$(mktemp -d); cd "$TMP"
git init -q && git config user.email a@b.c && git config user.name t
mkdir sub && cat > sub/foo.c <<'EOF'
struct Foo { char a; int b; char c; };
EOF
git add -A && git commit -q -m init
cat > sub/foo.c <<'EOF'
struct Foo { char a; int b; char c; double d; };
EOF
git add -A && git commit -q -m grow
/path/to/target/release/padlock diff sub/foo.c --against HEAD~1   # from inside the repo
cd / && /path/to/target/release/padlock diff "$TMP/sub/foo.c" --against HEAD~1   # from outside it — this exact case was broken twice (wrong repo resolved, then a Windows-only canonicalize bug)
rm -rf "$TMP"
```

Expect `~ Foo  12B -> 24B  (waste 6B -> 10B, score NN -> NN)`, not `+ Foo (new)` — the latter means the git-repo-root resolution broke again.

## Gotcha: this machine has a globally `cargo install`-ed `padlock`

`which padlock` resolves to `~/.cargo/bin/padlock`, separate from `./target/release/padlock`. This is what the VS Code extension actually calls (its default `padlock.executable` setting is just `"padlock"`, resolved via PATH) — **not** the freshly built release binary. After any CLI change meant to reach the extension, also run:

```bash
cargo install --path crates/padlock-cli --force
padlock --version   # confirm it now matches the commit you just built
```

Skipping this step was the root cause of a real incident (2026-10-10): the global binary sat 5 versions stale (`0.12.0` while source was at `0.17.0`), so the extension's on-save `--no-cache` flag — added in `0.14.0` — made every analysis call fail with a clap usage error, silently (see the VS Code verify skill for the rest of that story).

## Standard checks (run these, but they are not the verification)

```bash
cargo fmt && cargo fmt --check
cargo clippy --workspace -- -D warnings
cargo test --workspace
```
