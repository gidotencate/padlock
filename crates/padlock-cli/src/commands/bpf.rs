// padlock-cli/src/commands/bpf.rs
//
// `padlock bpf` — analyse eBPF object files or binaries containing a .BTF section.
//
// This is a thin wrapper around `padlock analyze` that:
//  1. Prints a one-line note explaining that BTF-derived layouts are compiler-accurate.
//  2. Delegates entirely to the existing analyze pipeline.
//
// All filtering, output format, and severity flags work identically.

use std::path::PathBuf;

use crate::commands::analyze::{self, AnalyzeOpts};
use crate::filter::{FailSeverity, FilterArgs};

pub fn run(
    paths: &[PathBuf],
    json: bool,
    sarif: bool,
    fail_on_severity: Option<FailSeverity>,
    filter: &FilterArgs,
) -> anyhow::Result<()> {
    // Only print the note when outputting human-readable text — skip for JSON/SARIF
    // so machine consumers don't have to strip it.
    if !json && !sarif {
        eprintln!(
            "padlock bpf: analysing BTF section — layouts reflect compiled types (compiler-accurate).\n\
             False-sharing findings on BPF map structs are directly actionable: pad to separate\n\
             frequently-updated map values onto distinct cache lines.\n"
        );
    }

    analyze::run(
        paths,
        AnalyzeOpts {
            json,
            sarif,
            markdown: false,
            cache_line_size: None,
            word_size: None,
            fail_on_severity,
            target: None,
            stdlib: None,
            show_skipped: false,
        },
        filter,
    )
}
