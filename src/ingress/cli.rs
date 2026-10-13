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
    /// Store and inspect requests. Execution workers are not implemented yet.
    Task {
        #[command(subcommand)]
        command: TaskCommand,
    },
}

#[derive(Subcommand)]
pub enum TaskCommand {
    /// Enqueue a request. Retry the same event with the same key and prompt.
    Add {
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
    /// Cancel a queued request. Repeated cancellation is safe.
    Cancel {
        id: i64,
    },
}
