use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about = "Ru41: Rust + Codex personal automation foundation")]
pub struct Cli {
    /// Path to local runtime data; never use a database inside your Vault.
    #[arg(long, global = true, default_value = ".ru41/state.db")]
    pub db: PathBuf,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Check local Codex CLI availability; does not invoke a model.
    Doctor,
    /// List registered runtime backends and their supported contracts.
    Backends,
    /// Execute queued analysis tasks through Codex (uses your Codex authentication).
    Worker {
        #[arg(long, default_value = "codex")]
        backend: String,
        /// Run at most one ready task, then exit.
        #[arg(long)]
        once: bool,
        #[arg(long, default_value = ".ru41/runs")]
        runs_dir: PathBuf,
        #[arg(long, default_value = "codex")]
        codex_bin: PathBuf,
        #[arg(long)]
        model: Option<String>,
        #[arg(long, default_value_t = 300, value_parser = clap::value_parser!(u64).range(1..=86400))]
        timeout_secs: u64,
    },
    /// Store and inspect analysis requests and their execution history.
    Task {
        #[command(subcommand)]
        command: TaskCommand,
    },
}

#[derive(Subcommand)]
pub enum TaskCommand {
    /// Enqueue a request. Retry the same event with the same key and prompt.
    Add {
        #[arg(long, default_value = "codex")]
        backend: String,
        #[arg(long)]
        key: String,
        #[arg(long)]
        prompt: String,
    },
    List {
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    Show {
        id: i64,
    },
    /// Show all execution attempts, including errors and local artifact paths.
    Attempts {
        id: i64,
    },
    /// Retry a failed task with a fresh attempt budget.
    Retry {
        id: i64,
    },
    /// Cancel a queued or running request. Repeated cancellation is safe.
    Cancel {
        id: i64,
    },
}
