use std::path::PathBuf;

use clap::{Parser, Subcommand};

pub mod cache;
pub mod config;
pub mod filter;
pub mod paths;

mod commands {
    pub mod analyze;
    pub mod bpf;
    pub mod check;
    pub mod diff;
    pub mod explain;
    pub mod fix;
    pub mod init;
    pub mod list;
    pub mod report;
    pub mod summary;
    pub mod watch;
}
mod output {
    pub mod terminal;
}

#[derive(Parser)]
#[command(
    name = "padlock",
    about = "Struct memory layout analyzer for C, C++, Rust, and Go",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Analyze one or more files or directories for struct layout issues
    Analyze {
        /// Paths to analyze: source files (.c .cpp .rs .go .zig), binaries, or directories
        #[arg(num_args = 1.., value_name = "PATH")]
        paths: Vec<PathBuf>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Output as SARIF (for CI / GitHub Code Scanning annotations)
        #[arg(long)]
        sarif: bool,
        /// Output as Markdown (suitable for CI step summaries or PR comment bots)
        #[arg(long)]
        markdown: bool,
        /// Override the cache-line size in bytes (e.g. 128 for Apple Silicon or POWER)
        #[arg(long, value_name = "BYTES")]
        cache_line_size: Option<usize>,
        /// Override the pointer/word size in bytes (e.g. 4 for 32-bit targets)
        #[arg(long, value_name = "BYTES")]
        word_size: Option<usize>,
        /// Exit with a non-zero status code if any finding meets or exceeds this severity
        /// (high, medium, or low). Useful for stricter CI gates.
        #[arg(long, value_name = "SEVERITY")]
        fail_on_severity: Option<filter::FailSeverity>,
        /// Target architecture as a Rust triple or short name
        /// (e.g. aarch64-apple-darwin, x86_64-unknown-linux-gnu, aarch64, wasm32).
        /// Overrides the arch.override config value.
        #[arg(long, value_name = "TRIPLE")]
        target: Option<String>,
        /// C++ standard library variant for type-size lookups.
        /// Affects sizes of std::string, std::mutex, etc.
        /// Choices: libstdc++ (GCC/Linux default), libc++ (Clang/macOS), msvc (Windows).
        #[arg(long, value_name = "VARIANT")]
        stdlib: Option<String>,
        #[command(flatten)]
        filter: filter::FilterArgs,
    },

    /// Show a project-level health summary: aggregate score, severity distribution,
    /// worst files, and worst structs. Designed for large codebases where `analyze`
    /// output is too verbose.
    Summary {
        /// Paths to analyze: source files, binaries, or directories
        #[arg(num_args = 1.., value_name = "PATH")]
        paths: Vec<PathBuf>,
        /// Number of worst files and structs to show (default: 5)
        #[arg(long, value_name = "N", default_value = "5")]
        top: usize,
        /// Override the cache-line size in bytes
        #[arg(long, value_name = "BYTES")]
        cache_line_size: Option<usize>,
        /// Override the pointer/word size in bytes
        #[arg(long, value_name = "BYTES")]
        word_size: Option<usize>,
        /// Target architecture as a Rust triple or short name
        #[arg(long, value_name = "TRIPLE")]
        target: Option<String>,
        #[command(flatten)]
        filter: filter::FilterArgs,
    },

    /// List all structs found in one or more files with basic stats
    List {
        /// Paths to analyze: source files, binaries, or directories
        #[arg(num_args = 1.., value_name = "PATH")]
        paths: Vec<PathBuf>,
        #[command(flatten)]
        filter: filter::FilterArgs,
    },

    /// Show a diff of original vs optimal field ordering
    Diff {
        /// Source files or directories to diff (binaries not supported)
        #[arg(num_args = 1.., value_name = "PATH")]
        paths: Vec<PathBuf>,
        /// Include only structs whose names match this regex pattern
        #[arg(long, short = 'F', value_name = "PATTERN")]
        filter: Option<String>,
    },

    /// Apply automatic field reordering to source files in-place
    Fix {
        /// Source files or directories to fix (binaries not supported)
        #[arg(num_args = 1.., value_name = "PATH")]
        paths: Vec<PathBuf>,
        /// Show the diff without writing any files
        #[arg(long)]
        dry_run: bool,
        /// Include only structs whose names match this regex pattern
        #[arg(long, short = 'F', value_name = "PATTERN")]
        filter: Option<String>,
    },

    /// Generate a full report (alias for analyze)
    Report {
        /// Paths to analyze: source files, binaries, or directories
        #[arg(num_args = 1.., value_name = "PATH")]
        paths: Vec<PathBuf>,
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        filter: filter::FilterArgs,
    },

    /// Watch a file or directory and re-analyse on every change
    Watch {
        /// Path to watch (source file, binary, or directory)
        path: PathBuf,
        /// Output results as JSON on each refresh
        #[arg(long)]
        json: bool,
    },

    /// Show a visual field-by-field memory layout table for each struct
    Explain {
        /// Paths to analyze: source files, binaries, or directories
        #[arg(num_args = 1.., value_name = "PATH")]
        paths: Vec<PathBuf>,
        /// Include only structs whose names match this regex pattern
        #[arg(long, short = 'F', value_name = "PATTERN")]
        filter: Option<String>,
    },

    /// Analyse eBPF object files or binaries that contain a .BTF section.
    ///
    /// This is an alias for `padlock analyze` that accepts the same paths and
    /// flags but prints a brief note reminding users that BTF-derived layouts
    /// reflect the compiled types, not the source, and that false-sharing
    /// findings for BPF map structs are directly actionable.
    ///
    /// Example: padlock bpf my_prog.bpf.o --json
    Bpf {
        /// eBPF object files or binaries with a .BTF ELF section
        #[arg(num_args = 1.., value_name = "PATH")]
        paths: Vec<PathBuf>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Output as SARIF
        #[arg(long)]
        sarif: bool,
        /// Exit non-zero when any finding meets or exceeds this severity
        #[arg(long, value_name = "SEVERITY")]
        fail_on_severity: Option<filter::FailSeverity>,
        #[command(flatten)]
        filter: filter::FilterArgs,
    },

    /// Generate a .padlock.toml configuration template in the current directory
    Init,

    /// Compare current layout findings against a saved baseline; fail only on regressions
    Check {
        /// Paths to analyze: source files, binaries, or directories
        #[arg(num_args = 1.., value_name = "PATH")]
        paths: Vec<PathBuf>,
        /// Path to baseline JSON file (created with --save-baseline)
        #[arg(long, value_name = "FILE")]
        baseline: Option<PathBuf>,
        /// Save current findings as the new baseline instead of comparing
        #[arg(long)]
        save_baseline: bool,
        /// Output comparison result as JSON
        #[arg(long)]
        json: bool,
        /// Target architecture as a Rust triple or short name
        #[arg(long, value_name = "TRIPLE")]
        target: Option<String>,
        #[command(flatten)]
        filter: filter::FilterArgs,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Analyze {
            paths,
            json,
            sarif,
            markdown,
            cache_line_size,
            word_size,
            fail_on_severity,
            target,
            stdlib,
            filter,
        } => commands::analyze::run(
            &paths,
            commands::analyze::AnalyzeOpts {
                json,
                sarif,
                markdown,
                cache_line_size,
                word_size,
                fail_on_severity,
                target,
                stdlib: stdlib.as_deref().and_then(parse_stdlib),
            },
            &filter,
        ),

        Commands::Summary {
            paths,
            top,
            cache_line_size,
            word_size,
            target,
            filter,
        } => commands::summary::run(&paths, top, cache_line_size, word_size, target, &filter),

        Commands::Bpf {
            paths,
            json,
            sarif,
            fail_on_severity,
            filter,
        } => commands::bpf::run(&paths, json, sarif, fail_on_severity, &filter),

        Commands::Init => commands::init::run(),

        Commands::List { paths, filter } => commands::list::run(&paths, &filter),

        Commands::Diff { paths, filter } => commands::diff::run(&paths, filter.as_deref()),

        Commands::Fix {
            paths,
            dry_run,
            filter,
        } => commands::fix::run(&paths, dry_run, filter.as_deref()),

        Commands::Report {
            paths,
            json,
            filter,
        } => commands::analyze::run(
            &paths,
            commands::analyze::AnalyzeOpts {
                json,
                sarif: false,
                markdown: false,
                cache_line_size: None,
                word_size: None,
                fail_on_severity: None,
                target: None,
                stdlib: None,
            },
            &filter,
        ),

        Commands::Watch { path, json } => commands::watch::run(&path, json),

        Commands::Explain { paths, filter } => commands::explain::run(&paths, filter.as_deref()),

        Commands::Check {
            paths,
            baseline,
            save_baseline,
            json,
            target,
            filter,
        } => commands::check::run(
            &paths,
            baseline.as_deref(),
            save_baseline,
            json,
            target,
            &filter,
        ),
    }
}

fn parse_stdlib(s: &str) -> Option<padlock_source::CppStdlib> {
    match s.to_ascii_lowercase().replace(['-', '_', '+'], "").as_str() {
        "libstdcpp" | "stdcpp" | "gcc" | "gnustl" => Some(padlock_source::CppStdlib::LibStdCpp),
        "libcpp" | "libc" | "clang" => Some(padlock_source::CppStdlib::LibCpp),
        "msvc" | "msstl" | "ms" => Some(padlock_source::CppStdlib::Msvc),
        other => {
            eprintln!(
                "padlock: warning: unknown --stdlib '{other}', \
                 expected libstdc++, libc++, or msvc — using libstdc++ default"
            );
            None
        }
    }
}
