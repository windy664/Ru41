use anyhow::{Context, Result};
use clap::Parser;
use ru41::{
    adapters::{
        agent::codex,
        database::{postgres::PostgresTaskStore, sqlite::SqliteTaskStore},
    },
    app::{
        agent_service::BackendRegistry,
        store::TaskStore,
        task_service::TaskService,
        worker::{Worker, WorkerConfig},
    },
    ingress::{
        cli::{Cli, Command, QqCommand, TaskCommand},
        qq::{QqBot, api::QqApiClient},
    },
};
use std::{path::PathBuf, sync::Arc};
use tokio::sync::watch;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let store_config = (cli.store, cli.pg_url, cli.db);
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
            // 在切换子进程工作目录前，先解析显式的相对可执行路径。
            let executable = if codex_bin.components().count() > 1 {
                std::fs::canonicalize(codex_bin)?
            } else {
                codex_bin
            };
            let backends = registry(codex::CodexAgent { executable, model })?;
            let mut worker = Worker {
                store: open_store(&store_config)?,
                agent: backends.get(&backend)?,
                config: WorkerConfig {
                    worker_id: format!("worker-{}", std::process::id()),
                    runs_dir,
                    timeout: std::time::Duration::from_secs(timeout_secs),
                    lease_secs: 30,
                    retry_delay_secs: 10,
                },
            };
            let receiver = shutdown_channel()?;
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
            return Ok(());
        }
        Command::Qq { command } => match command {
            QqCommand::Run {
                credentials,
                backend,
            } => {
                registry(codex::CodexAgent::default())?.get(&backend)?;
                let bot = QqBot {
                    api: Arc::new(QqApiClient::new(credentials.app_id, credentials.app_secret)),
                    store: open_store(&store_config)?,
                    backend,
                };
                let receiver = shutdown_channel()?;
                bot.run(receiver).await?;
                return Ok(());
            }
            QqCommand::Send {
                credentials,
                group_openid,
                text,
            } => {
                let api = QqApiClient::new(credentials.app_id, credentials.app_secret);
                serde_json::to_value(api.send_group_text(&group_openid, &text, None, 1).await?)?
            }
        },
        Command::Task { command } => {
            let mut service = TaskService::new(open_store(&store_config)?);
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

fn open_store(
    (kind, pg_url, db): &(String, Option<String>, PathBuf),
) -> Result<Arc<dyn TaskStore>> {
    match kind.as_str() {
        "sqlite" => Ok(Arc::new(SqliteTaskStore::open(db)?)),
        "postgres" => {
            let url = pg_url
                .as_deref()
                .context("--store postgres requires --pg-url or RU41_PG_URL")?;
            // postgres 同步客户端内部会 block_on，需离开当前 runtime 上下文。
            Ok(Arc::new(tokio::task::block_in_place(|| {
                PostgresTaskStore::open(url)
            })?))
        }
        other => anyhow::bail!("unknown store backend: {other}"),
    }
}

fn registry(codex: codex::CodexAgent) -> Result<BackendRegistry> {
    let mut backends = BackendRegistry::default();
    backends.register(codex)?;
    Ok(backends)
}

/// SIGINT/SIGTERM 触发的优雅关闭信号。
fn shutdown_channel() -> Result<watch::Receiver<bool>> {
    let (shutdown, receiver) = watch::channel(false);
    tokio::spawn(async move {
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
    Ok(receiver)
}
