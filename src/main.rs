use anyhow::{Context, Result};
use clap::Parser;
use ru41::{
    adapters::{
        agent::codex::{self, CodexAgent},
        database::{postgres::PostgresTaskStore, sqlite::SqliteTaskStore},
    },
    app::{
        agent_service::BackendRegistry,
        config::Config,
        notify_service::NotifyDispatcher,
        sanitize::{NoopSanitizer, RandomSanitizer, Sanitizer},
        store::TaskStore,
        task_service::TaskService,
        worker::{Worker, WorkerConfig},
    },
    ingress::{
        cli::{Cli, Command, QqCommand, QqCredentials, TaskCommand},
        qq::{QqBot, QqNotifier, api::QqApiClient},
    },
};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::watch;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let (config, config_path) = Config::load(cli.config.as_deref())?;
    // 存储解析在移动 command 之前完成（CLI > 环境变量 > 配置文件 > 默认值）。
    let store_kind = cli
        .store
        .clone()
        .or_else(|| config.store.kind.clone())
        .unwrap_or_else(|| "sqlite".into());
    let store_db = cli
        .db
        .clone()
        .or_else(|| config.store.path.clone())
        .unwrap_or_else(|| PathBuf::from(".ru41/state.db"));
    let store_pg_url = cli.pg_url.clone().or_else(|| config.store.url.clone());
    if let Some(path) = &config_path {
        eprintln!("[config] using {}", path.display());
    }
    let store_config = (store_kind, store_db, store_pg_url);
    let output = match cli.command {
        Command::Doctor => serde_json::json!({
            "codex_version": codex::version()?,
            "model_invoked": false,
            "authentication_checked": false,
            "stage": "task-execution"
        }),
        Command::Backends => serde_json::to_value(
            registry(&codex_agent(
                default_codex_bin(&config),
                default_model(&config),
            )?)?
            .descriptors(),
        )?,
        Command::Worker {
            once,
            runs_dir,
            codex_bin,
            model,
            no_sanitize,
            timeout_secs,
        } => {
            let bin = codex_bin
                .or_else(|| config.agent.codex.bin.clone())
                .unwrap_or_else(|| PathBuf::from("codex"));
            let model = model.or_else(|| config.agent.codex.model.clone());
            let backends = registry(&codex_agent(bin, model)?)?;
            let sanitize = if no_sanitize {
                false
            } else {
                config.worker.sanitize.unwrap_or(true)
            };
            let sanitizer: Arc<dyn Sanitizer> = if sanitize {
                Arc::new(RandomSanitizer::default())
            } else {
                Arc::new(NoopSanitizer)
            };
            let mut worker = Worker {
                store: open_store(&store_config)?,
                backends: Arc::new(backends),
                sanitizer,
                config: WorkerConfig {
                    worker_id: format!("worker-{}", std::process::id()),
                    runs_dir: runs_dir
                        .or_else(|| config.worker.runs_dir.clone())
                        .unwrap_or_else(|| PathBuf::from(".ru41/runs")),
                    timeout: Duration::from_secs(
                        timeout_secs.or(config.worker.timeout_secs).unwrap_or(300),
                    ),
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
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
            return Ok(());
        }
        Command::Qq { command } => match command {
            QqCommand::Run {
                credentials,
                backend,
            } => {
                let backend = backend.unwrap_or_else(|| "codex".into());
                let api = Arc::new(qq_api(&credentials, &config)?);
                let store = open_store(&store_config)?;
                let bot = QqBot {
                    api: api.clone(),
                    store: store.clone(),
                    backend,
                };
                let dispatcher = NotifyDispatcher {
                    store,
                    channels: vec![Arc::new(QqNotifier::new(api))],
                    batch: 20,
                    interval: Duration::from_secs(2),
                };
                let receiver = shutdown_channel()?;
                tokio::select! {
                    result = bot.run(receiver.clone()) => result?,
                    result = dispatcher.run_loop(receiver) => result?,
                }
                return Ok(());
            }
            QqCommand::Send {
                credentials,
                group_openid,
                text,
            } => {
                let api = qq_api(&credentials, &config)?;
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
                    registry(&codex_agent(
                        default_codex_bin(&config),
                        default_model(&config),
                    )?)?
                    .get(&backend)?;
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

/// CLI > 环境变量（clap env）> 配置文件 > 默认值。
fn open_store(
    (kind, db, pg_url): &(String, PathBuf, Option<String>),
) -> Result<Arc<dyn TaskStore>> {
    match kind.as_str() {
        "sqlite" => Ok(Arc::new(SqliteTaskStore::open(db)?)),
        "postgres" => {
            let url = pg_url.as_deref().context(
                "--store postgres requires --pg-url, RU41_PG_URL or store.url in config",
            )?;
            // postgres 同步客户端内部会 block_on，需离开当前 runtime 上下文。
            Ok(Arc::new(tokio::task::block_in_place(|| {
                PostgresTaskStore::open(url)
            })?))
        }
        other => anyhow::bail!("unknown store backend: {other}"),
    }
}

fn default_codex_bin(config: &Config) -> PathBuf {
    config
        .agent
        .codex
        .bin
        .clone()
        .unwrap_or_else(|| PathBuf::from("codex"))
}

fn default_model(config: &Config) -> Option<String> {
    config.agent.codex.model.clone()
}

fn codex_agent(bin: PathBuf, model: Option<String>) -> Result<CodexAgent> {
    // 在切换子进程工作目录前，先解析显式的相对可执行路径。
    let executable = if bin.components().count() > 1 {
        std::fs::canonicalize(bin)?
    } else {
        bin.clone()
    };
    Ok(CodexAgent { executable, model })
}

fn registry(codex: &CodexAgent) -> Result<BackendRegistry> {
    let mut backends = BackendRegistry::default();
    backends.register(CodexAgent {
        executable: codex.executable.clone(),
        model: codex.model.clone(),
    })?;
    Ok(backends)
}

fn qq_api(credentials: &QqCredentials, config: &Config) -> Result<QqApiClient> {
    let app_id = credentials
        .app_id
        .clone()
        .or_else(|| config.channel.qq.app_id.clone())
        .context("QQ app id required: --qq-app-id, RU41_QQ_APP_ID or channel.qq.app_id")?;
    let app_secret = credentials
        .app_secret
        .clone()
        .or_else(|| config.channel.qq.app_secret.clone())
        .context(
            "QQ app secret required: --qq-app-secret, RU41_QQ_APP_SECRET or channel.qq.app_secret",
        )?;
    Ok(QqApiClient::new(app_id, app_secret))
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
