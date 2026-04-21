use clap::{Parser, Subcommand};
use std::process::ExitCode;
use vp_ccws::commands;

#[derive(Parser)]
#[command(
    name = "ccws",
    about = "Claude Code Workspace - Git clone-based workspace manager"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a new worker environment (clone + symlink + setup)
    New {
        /// Worker name
        name: String,
        /// Branch name to create
        branch: String,
        /// Overwrite existing worker
        #[arg(long, short)]
        force: bool,
    },
    /// Fork current dirty state into a new worker environment
    Fork {
        /// Worker name
        name: String,
        /// Branch name to create
        branch: String,
        /// Overwrite existing worker
        #[arg(long, short)]
        force: bool,
    },
    /// List all worker environments
    Ls,
    /// Print the path to a worker environment
    Path {
        /// Worker name
        name: String,
    },
    /// Remove a worker environment
    Rm {
        /// Worker name (or --all --force)
        name: Option<String>,
        /// Remove all workers (requires --force)
        #[arg(long)]
        all: bool,
        /// Force removal without confirmation
        #[arg(long, short)]
        force: bool,
    },
    /// Show status of all worker environments
    Status,
    /// Remove workers whose branch is merged into main
    Cleanup {
        /// Actually delete (without this flag, only shows what would be deleted)
        #[arg(long, short)]
        force: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::New {
            name,
            branch,
            force,
        } => commands::new_worker(&name, &branch, force),
        Commands::Fork {
            name,
            branch,
            force,
        } => commands::fork_worker(&name, &branch, force),
        Commands::Ls => commands::list_workers(),
        Commands::Path { name } => commands::worker_path(&name),
        Commands::Rm { name, all, force } => commands::remove_worker(name.as_deref(), all, force),
        Commands::Status => commands::status_workers(),
        Commands::Cleanup { force } => commands::cleanup_workers(force),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("エラー: {e}");
            ExitCode::FAILURE
        }
    }
}
