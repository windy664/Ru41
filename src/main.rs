use anyhow::{Context, Result};
use clap::Parser;
use ru41::bootstrap::agents::{AgentOverrides, registry};
use ru41::bootstrap::assembly::{
    default_notify_route, ecard_tracker, notify_channels, paper_tracker, qq_api, rss_tracker,
};
use ru41::{
    adapters::{
        agent::codex,
        database::{postgres::PostgresTaskStore, sqlite::SqliteTaskStore},
        sources::arxiv::ArxivClient,
    },
    app::task::TaskService,
    bootstrap::config::Config,
    ingress::{
        cli::{Cli, Command, EcardCommand, PapersCommand, QqCommand, RssCommand, TaskCommand},
        qq::QqBot,
    },
    ports::storage::TaskStore,
    privacy::{NoopSanitizer, RandomSanitizer, Sanitizer},
    runtime::dispatcher::NotifyDispatcher,
    runtime::worker::{Worker, WorkerConfig},
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
        Command::Serve { listen } => {
            // 令牌优先级：环境变量 > 配置文件。
            let token = std::env::var("RU41_API_TOKEN")
                .ok()
                .or_else(|| config.api.token.clone())
                .context("API token required: RU41_API_TOKEN or api.token in config")?;
            let listen = listen
                .map(|addr| addr.to_string())
                .or_else(|| config.api.listen.clone())
                .unwrap_or_else(|| "127.0.0.1:4141".into());
            let listen: std::net::SocketAddr =
                listen.parse().context("invalid api listen address")?;
            let state = ru41::ingress::http::ApiState::new(
                open_store(&store_config)?,
                Arc::new(registry(&config.agent, AgentOverrides::default())?),
                token,
            )?;
            let listener = tokio::net::TcpListener::bind(listen).await?;
            let mut shutdown = shutdown_channel()?;
            axum::serve(listener, ru41::ingress::http::router(state))
                .with_graceful_shutdown(async move {
                    let _ = shutdown.changed().await;
                })
                .await?;
            return Ok(());
        }
        Command::Doctor => serde_json::json!({
            "codex_version": codex::version()?,
            "model_invoked": false,
            "authentication_checked": false,
            "stage": "task-execution"
        }),
        Command::Backends => {
            serde_json::to_value(registry(&config.agent, AgentOverrides::default())?.descriptors())?
        }
        Command::Worker {
            once,
            runs_dir,
            codex_bin,
            model,
            no_sanitize,
            timeout_secs,
        } => {
            let backends = registry(
                &config.agent,
                AgentOverrides {
                    codex_bin,
                    codex_model: model,
                },
            )?;
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
                bind_secret,
            } => {
                let backend = backend.unwrap_or_else(|| "codex".into());
                let bind_secret = bind_secret.or_else(|| config.channel.qq.bind_secret.clone());
                let api = Arc::new(qq_api(&credentials, &config)?);
                let store = open_store(&store_config)?;
                let bot = QqBot {
                    api: api.clone(),
                    store: store.clone(),
                    backend,
                    bind_secret,
                    arxiv_client: Some(ArxivClient::new()),
                    papers_interests: config
                        .sources
                        .arxiv
                        .interests
                        .clone()
                        .filter(|s| !s.trim().is_empty()),
                };
                let dispatcher = NotifyDispatcher {
                    store,
                    channels: notify_channels(&config, Some(api))?,
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
        Command::Notify => {
            let channels = notify_channels(&config, None)?;
            anyhow::ensure!(
                !channels.is_empty(),
                "no notify channel configured (e.g. channel.feishu.webhook_url)"
            );
            let dispatcher = NotifyDispatcher {
                store: open_store(&store_config)?,
                channels,
                batch: 20,
                interval: Duration::from_secs(2),
            };
            dispatcher.run_loop(shutdown_channel()?).await?;
            return Ok(());
        }
        Command::Papers { command } => match command {
            PapersCommand::Poll => {
                let tracker = paper_tracker(&config, open_store(&store_config)?)?;
                serde_json::to_value(tracker.poll_once().await?)?
            }
            PapersCommand::Run => {
                let tracker = paper_tracker(&config, open_store(&store_config)?)?;
                let tracker_ref = &tracker;
                ru41::ingress::schedule::run_periodic(
                    "papers",
                    tracker.interval(),
                    move || {
                        let tracker = tracker_ref;
                        async move { Ok(serde_json::to_value(tracker.poll_once().await?)?) }
                    },
                    shutdown_channel()?,
                )
                .await?;
                return Ok(());
            }
        },
        Command::Rss { command } => match command {
            RssCommand::Poll => {
                let tracker = rss_tracker(&config, open_store(&store_config)?)?;
                serde_json::to_value(tracker.poll_once().await?)?
            }
            RssCommand::Run => {
                let tracker = rss_tracker(&config, open_store(&store_config)?)?;
                let tracker_ref = &tracker;
                ru41::ingress::schedule::run_periodic(
                    "rss",
                    tracker.interval(),
                    move || {
                        let tracker = tracker_ref;
                        async move { Ok(serde_json::to_value(tracker.poll_once().await?)?) }
                    },
                    shutdown_channel()?,
                )
                .await?;
                return Ok(());
            }
        },
        Command::Ecard { command } => match command {
            EcardCommand::Poll => {
                let tracker = ecard_tracker(&config, open_store(&store_config)?)?;
                serde_json::to_value(tracker.poll_once().await?)?
            }
            EcardCommand::Run => {
                let tracker = ecard_tracker(&config, open_store(&store_config)?)?;
                let tracker_ref = &tracker;
                ru41::ingress::schedule::run_periodic(
                    "ecard",
                    tracker.interval(),
                    move || {
                        let tracker = tracker_ref;
                        async move { Ok(serde_json::to_value(tracker.poll_once().await?)?) }
                    },
                    shutdown_channel()?,
                )
                .await?;
                return Ok(());
            }
        },
        Command::Task { command } => {
            let mut service = TaskService::new(open_store(&store_config)?);
            match command {
                TaskCommand::Add {
                    key,
                    prompt,
                    backend,
                    notify,
                } => {
                    registry(&config.agent, AgentOverrides::default())?.get(&backend)?;
                    match notify {
                        false => {
                            serde_json::to_value(service.enqueue_for(&key, &prompt, &backend)?)?
                        }
                        true => {
                            let route = default_notify_route(&config)?.context(
                                "no default notify channel configured ([notify].channel)",
                            )?;
                            serde_json::to_value(
                                service.enqueue_with_notify(&key, &prompt, &backend, &route)?,
                            )?
                        }
                    }
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
