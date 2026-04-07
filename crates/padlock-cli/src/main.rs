use std::path::PathBuf;

use clap::{Parser, Subcommand};

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
#[command(name = "padlock", about = "Struct memory layout analyzer")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Analyze a binary or source file for struct layout issues
    Analyze {
        /// Path to the binary (.o, ELF) or source file (.c, .cpp, .rs, .go)
        path: PathBuf,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Output as SARIF (for CI / GitHub annotations)
        #[arg(long)]
        sarif: bool,
    },
    /// List all structs found in a file with basic stats
    List { path: PathBuf },
    /// Show a diff of original vs optimal field ordering
    Diff { path: PathBuf },
    /// Apply automatic field reordering to a source file
    Fix {
        path: PathBuf,
        /// Show the diff without writing any files
        #[arg(long)]
        dry_run: bool,
    },
    /// Generate a full report
    Report {
        path: PathBuf,
        #[arg(long)]
        json: bool,
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
        Commands::Analyze { path, json, sarif } => commands::analyze::run(&path, json, sarif),
        Commands::List { path } => commands::list::run(&path),
        Commands::Diff { path } => commands::diff::run(&path),
        Commands::Fix { path, dry_run } => commands::fix::run(&path, dry_run),
        Commands::Report { path, json } => commands::analyze::run(&path, json, false),
        Commands::Watch { path, json } => commands::watch::run(&path, json),
    }
}
