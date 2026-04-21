# Contributing to padlock

## Getting started

```bash
git clone https://github.com/gidotencate/padlock
cd padlock
cargo build
cargo test
```

Requires Rust 1.88+.

## Commit workflow

Every commit must pass before it lands:

```bash
cargo fmt --all
cargo fmt --all --check
cargo clippy --workspace -- -D warnings
cargo test
```

CI enforces `fmt --check` and `clippy -D warnings` — PRs that fail either will not be merged.

## Branch and PR conventions

- Branch from `main`; target `main`
- One logical change per PR
- Add or update tests when changing analysis logic
- Add a `CHANGELOG.md` entry and bump versions when warranted (see `docs/publishing.md`)

## Adding a test

Most analysis tests live in `crates/padlock-core/` and `crates/padlock-source/`. Construct a source string, parse it, run `Report::from_layouts`, and assert on the findings.

For tree-sitter work: write a temporary test that prints `node.to_sexp()` to confirm node kinds, then remove it.

## Questions

Open a GitHub issue.
