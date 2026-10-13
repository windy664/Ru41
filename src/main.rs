use anyhow::Result;
use clap::Parser;
use ru41::{
    adapters::{codex, sqlite::TaskStore},
    app::task_service::TaskService,
    ingress::cli::{Cli, Command, TaskCommand},
};

fn main() -> Result<()> {
    let cli = Cli::parse();
    let output = match cli.command {
        Command::Doctor => serde_json::json!({
            "codex_version": codex::version()?,
            "model_invoked": false,
            "authentication_checked": false,
            "stage": "foundation"
        }),
        Command::Task { command } => {
            let mut service = TaskService::new(TaskStore::open(&cli.db)?);
            match command {
                TaskCommand::Add { key, prompt } => {
                    serde_json::to_value(service.enqueue(&key, &prompt)?)?
                }
                TaskCommand::List { limit } => serde_json::to_value(service.list(limit)?)?,
                TaskCommand::Show { id } => serde_json::to_value(service.get(id)?)?,
                TaskCommand::Cancel { id } => serde_json::to_value(service.cancel(id)?)?,
            }
        }
    };
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
