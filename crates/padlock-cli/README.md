# padlock

**The lint pass for struct memory layout** — catches padding waste, false sharing, and cache locality problems at the source level, before they cost you at runtime.

Supports C, C++, Rust, and Go. CLI-first and CI-ready.

## Install

```bash
cargo install padlock-cli
```

## Quick start

```bash
# Analyze source files
padlock analyze src/

# Analyze a compiled binary (most accurate — reads real compiler offsets)
padlock analyze target/debug/myapp

# Show a visual field layout table with padding gaps
padlock explain src/connection.rs Connection

# Reorder fields in-place to the optimal layout
padlock fix src/connection.rs

# Rust projects: analyze via cargo
cargo padlock
```

## Documentation

Full documentation, language support tables, CI integration, and configuration reference are in the [main README](https://github.com/gidotencate/padlock).
