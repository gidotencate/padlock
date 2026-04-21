# padlock

**The lint pass for struct memory layout** — catches padding waste, false sharing, and cache locality problems at the source level, before they cost you at runtime.

Supports C, C++, Rust, Go, and Zig. CLI-first and CI-ready.

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

# Analyze a Windows PDB file
padlock analyze myapp.pdb

# Analyze an eBPF object file or raw BTF blob
padlock bpf myprogram.bpf.o
padlock bpf /sys/kernel/btf/vmlinux

# Show a visual field layout table with padding gaps
padlock explain src/connection.rs Connection

# Reorder fields in-place to the optimal layout
padlock fix src/connection.rs

# Preview the reorderings without writing any files
padlock fix --dry-run src/connection.rs

# Write a .bak backup before rewriting (opt-in)
padlock fix --backup src/connection.rs

# Rust projects: analyze via cargo
cargo padlock
```

## Documentation

Full documentation, language support tables, CI integration, and configuration reference are in the [main README](https://github.com/gidotencate/padlock).
