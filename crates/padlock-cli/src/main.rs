use std::path::PathBuf;

use clap::{Parser, Subcommand};

pub mod config;
pub mod filter;
pub mod paths;

mod commands {
    pub mod analyze;
    pub mod diff;
    pub mod fix;
    pub mod list;
    pub mod report;
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
        /// Paths to analyze: source files (.c .cpp .rs .go), binaries, or directories
        #[arg(num_args = 1.., value_name = "PATH")]
        paths: Vec<PathBuf>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Output as SARIF (for CI / GitHub Code Scanning annotations)
        #[arg(long)]
        sarif: bool,
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
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Analyze {
            paths,
            json,
            sarif,
            filter,
        } => commands::analyze::run(&paths, json, sarif, &filter),

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
        } => commands::analyze::run(&paths, json, false, &filter),

        Commands::Watch { path, json } => commands::watch::run(&path, json),
    }
}
