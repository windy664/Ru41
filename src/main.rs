use anyhow::Result;
use clap::Parser;
use ru41::{
    adapters::{codex, sqlite::SqliteTaskStore},
    app::{
        agent_service::BackendRegistry,
        task_service::TaskService,
        worker::{Worker, WorkerConfig},
    },
    ingress::cli::{Cli, Command, TaskCommand},
};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let output = match cli.command {
        Command::Doctor => serde_json::json!({
            "codex_version": codex::version()?,
            "model_invoked": false,
            "authentication_checked": false,
            "stage": "task-execution"
        }),
        Command::Backends => {
            serde_json::to_value(registry(codex::CodexAgent::default())?.descriptors())?
        }
        Command::Worker {
            backend,
            once,
            runs_dir,
            codex_bin,
            model,
            timeout_secs,
        } => {
            // Resolve explicit relative executable paths before changing the child cwd.
            let executable = if codex_bin.components().count() > 1 {
                std::fs::canonicalize(codex_bin)?
            } else {
                codex_bin
            };
            let backends = registry(codex::CodexAgent { executable, model })?;
            let mut worker = Worker {
                store: Arc::new(SqliteTaskStore::open(&cli.db)?),
                agent: backends.get(&backend)?,
                config: WorkerConfig {
                    worker_id: format!("worker-{}", std::process::id()),
                    runs_dir,
                    timeout: std::time::Duration::from_secs(timeout_secs),
                    lease_secs: 30,
                    retry_delay_secs: 10,
                },
            };
            let (shutdown, receiver) = tokio::sync::watch::channel(false);
            let signal_task = tokio::spawn(async move {
                #[cfg(unix)]
                {
                    let mut terminate =
                        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
                    tokio::select! { r = tokio::signal::ctrl_c() => { r?; }, _ = terminate.recv() => {} }
                }
                #[cfg(not(unix))]
                tokio::signal::ctrl_c().await?;
                let _ = shutdown.send(true);
                Ok::<_, std::io::Error>(())
            });
            loop {
                if *receiver.borrow() {
                    break;
                }
                let report = worker.run_once(receiver.clone()).await?;
                if let Some(report) = &report {
                    println!("{}", serde_json::to_string(report)?);
                } else if once {
                    println!("{{\"status\":\"idle\"}}");
                }
                if once {
                    break;
                }
                if report.is_none() {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            }
            signal_task.abort();
            return Ok(());
        }
        Command::Task { command } => {
            let mut service = TaskService::new(Arc::new(SqliteTaskStore::open(&cli.db)?));
            match command {
                TaskCommand::Add {
                    key,
                    prompt,
                    backend,
                } => {
                    registry(codex::CodexAgent::default())?.get(&backend)?;
                    serde_json::to_value(service.enqueue_for(&key, &prompt, &backend)?)?
                }
                TaskCommand::List { limit } => serde_json::to_value(service.list(limit)?)?,
                TaskCommand::Show { id } => serde_json::to_value(service.get(id)?)?,
                TaskCommand::Cancel { id } => serde_json::to_value(service.cancel(id)?)?,
                TaskCommand::Retry { id } => serde_json::to_value(service.retry(id)?)?,
                TaskCommand::Attempts { id } => serde_json::to_value(service.attempts(id)?)?,
            }
        }
    };
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn registry(codex: codex::CodexAgent) -> Result<BackendRegistry> {
    let mut backends = BackendRegistry::default();
    backends.register(codex)?;
    Ok(backends)
}
