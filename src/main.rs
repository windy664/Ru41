use anyhow::{Context, Result};
use clap::Parser;
use ru41::bootstrap::agents::{AgentOverrides, registry};
use ru41::bootstrap::assembly::{
    default_notify_route, knowledge_service, notifier_for_route, notify_channels, qq_api,
    qq_command_handlers, qq_image_consumers, scheduled_jobs, todo_notify_route, tracker_job,
    youtube_client, youtube_prep_route,
};
use ru41::bootstrap::store::{open_store, resolve as resolve_store};
use ru41::{
    app::{schedule::ScheduleService, task::TaskService, todo::TodoService},
    bootstrap::config::Config,
    channels::qq::{QqBot, ReplySequencer},
    domain::schedule::{EventInput, EventSource, parse_event_when},
    domain::todo::{TodoInput, parse_due},
    ingress::cli::{
        ChaoxingCommand, Cli, Command, EventCommand, JwxtCommand, KnowledgeCommand, QqCommand,
        ScheduleCommand, TaskCommand, TeachermateCommand, TodoCommand, TrackerCommand,
        YoutubeCommand,
    },
    ingress::cli_ops,
    modules::youtube::{YoutubeSection, qq_command::parse_video_id, service::prep_video_task},
    privacy::{NoopSanitizer, RandomSanitizer, Sanitizer},
    runtime::dispatcher::NotifyDispatcher,
    runtime::worker::{Worker, WorkerConfig},
};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::watch;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Some(outcome) = cli_ops::try_run(&cli).await? {
        println!("{}", serde_json::to_string_pretty(&outcome.value)?);
        if !outcome.success {
            std::process::exit(1);
        }
        return Ok(());
    }
    let (config, config_path) = Config::load(cli.config.as_deref())?;
    let store_config = resolve_store(
        cli.store.clone(),
        cli.db.clone(),
        cli.pg_url.clone(),
        &config,
    );
    if let Some(path) = &config_path {
        eprintln!("[config] using {}", path.display());
    }
    // 纯 tracker 子命令（papers/rss/projects/ecard/zhihuishu/obsidian/blog）
    // 分支体完全一致，查表统一分发，不进下面的大 match。
    if let Some((name, tracker_command)) = cli.command.as_tracker() {
        let output =
            tracker_or_exit(tracker_cli(name, tracker_command, &config, &store_config).await?)?;
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }
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
        Command::Init { .. }
        | Command::Doctor { .. }
        | Command::Config { .. }
        | Command::Status
        | Command::Dump => {
            unreachable!("handled by cli_ops")
        }
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
                // 知识库初始化失败（模型缓存损坏等）不拖垮 worker，降级为无注入。
                knowledge: knowledge_service(&config).unwrap_or_else(|error| {
                    eprintln!("[knowledge] disabled: {error:#}");
                    None
                }),
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
                let command_handlers = qq_command_handlers(&config, &backend)?;
                let image_consumers = qq_image_consumers(&config)?;
                // 回执与终态/进度通知共享序号器，同 msg_id 被动回复 seq 不撞车。
                let sequencer = Arc::new(ReplySequencer::default());
                let bot = QqBot {
                    api: api.clone(),
                    store: store.clone(),
                    backend,
                    bind_secret,
                    command_handlers,
                    image_consumers,
                    sequencer: sequencer.clone(),
                };
                let dispatcher = NotifyDispatcher {
                    store,
                    channels: notify_channels(&config, Some(api), Some(sequencer))?,
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
            QqCommand::Doctor { .. } | QqCommand::Onboard => unreachable!("handled by cli_ops"),
        },
        Command::Notify => {
            let channels = notify_channels(&config, None, None)?;
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
        Command::Papers { .. }
        | Command::Rss { .. }
        | Command::Projects { .. }
        | Command::Ecard { .. }
        | Command::Zhihuishu { .. }
        | Command::Obsidian { .. }
        | Command::Blog { .. }
        | Command::Pagewatch { .. }
        | Command::Competitions { .. }
        | Command::Jobs { .. }
        | Command::Programs { .. }
        | Command::Research { .. } => {
            unreachable!("handled by tracker dispatch above")
        }
        Command::Youtube { command } => match command {
            YoutubeCommand::Prep { url } => {
                let Some(video_id) = parse_video_id(&url) else {
                    anyhow::bail!(
                        "无法从「{url}」解析视频 ID（支持 watch / youtu.be / shorts 链接或 11 位 ID）"
                    );
                };
                let youtube = &config.sources.youtube;
                let message = prep_video_task(
                    &open_store(&store_config)?,
                    &youtube_client(&config),
                    &video_id,
                    youtube.interests.as_deref().unwrap_or(""),
                    youtube
                        .level
                        .as_deref()
                        .unwrap_or(YoutubeSection::DEFAULT_LEVEL),
                    youtube
                        .max_transcript_chars
                        .unwrap_or(YoutubeSection::DEFAULT_MAX_TRANSCRIPT_CHARS),
                    youtube.backend.as_deref().unwrap_or("codex"),
                    youtube_prep_route(&config)?.as_ref(),
                )
                .await?;
                serde_json::json!({ "video_id": video_id, "message": message })
            }
            YoutubeCommand::Poll => tracker_or_exit(
                tracker_cli("youtube", TrackerCommand::Poll, &config, &store_config).await?,
            )?,
            YoutubeCommand::Run => {
                tracker_cli("youtube", TrackerCommand::Run, &config, &store_config).await?;
                return Ok(());
            }
        },
        Command::Chaoxing { command } => match command {
            ChaoxingCommand::Poll => tracker_or_exit(
                tracker_cli("chaoxing", TrackerCommand::Poll, &config, &store_config).await?,
            )?,
            ChaoxingCommand::Work => {
                let tracker = ru41::modules::chaoxing::build_work_tracker(
                    &config,
                    open_store(&store_config)?,
                )?;
                serde_json::to_value(tracker.poll_once().await?)?
            }
            ChaoxingCommand::Run => {
                tracker_cli("chaoxing", TrackerCommand::Run, &config, &store_config).await?;
                return Ok(());
            }
            ChaoxingCommand::Sign {
                activity_id,
                lat,
                lon,
                addr,
                enc,
                photo,
            } => {
                let tracker =
                    ru41::modules::chaoxing::build_tracker(&config, open_store(&store_config)?)?;
                serde_json::to_value(
                    tracker
                        .sign_manual(
                            &activity_id,
                            lat,
                            lon,
                            addr.as_deref(),
                            enc.as_deref(),
                            photo.as_deref(),
                        )
                        .await?,
                )?
            }
        },
        Command::Teachermate { command } => match command {
            TeachermateCommand::Poll => tracker_or_exit(
                tracker_cli("teachermate", TrackerCommand::Poll, &config, &store_config).await?,
            )?,
            TeachermateCommand::Run => {
                tracker_cli("teachermate", TrackerCommand::Run, &config, &store_config).await?;
                return Ok(());
            }
            TeachermateCommand::Work => {
                let tracker = ru41::modules::teachermate::build_work_tracker(
                    &config,
                    open_store(&store_config)?,
                )?;
                serde_json::to_value(tracker.poll_once().await?)?
            }
            TeachermateCommand::Bind { link } => {
                let openid_path = config.sources.teachermate.openid_path_or_default();
                ru41::modules::teachermate::cli::bind_json(&link, &openid_path).await?
            }
            TeachermateCommand::Sign { sign_id, lat, lon } => {
                let tracker =
                    ru41::modules::teachermate::build_tracker(&config, open_store(&store_config)?)?;
                serde_json::to_value(tracker.sign_manual(sign_id, lat, lon).await?)?
            }
        },
        Command::Jwxt { command } => match command {
            JwxtCommand::Poll => tracker_or_exit(
                tracker_cli("jwxt", TrackerCommand::Poll, &config, &store_config).await?,
            )?,
            JwxtCommand::Run => {
                tracker_cli("jwxt", TrackerCommand::Run, &config, &store_config).await?;
                return Ok(());
            }
            JwxtCommand::Login { captcha } => {
                let tracker =
                    ru41::modules::jwxt::build_tracker(&config, open_store(&store_config)?)?;
                ru41::modules::jwxt::cli::login_json(&tracker, captcha.as_deref()).await?
            }
            JwxtCommand::Classes { when } => ru41::modules::jwxt::cli::classes_json(
                open_store(&store_config)?,
                &config.sources.jwxt,
                &when,
            )?,
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
        Command::Todo { command } => {
            let options = config.todo.options()?;
            let service = TodoService::new(open_store(&store_config)?, options);
            let now = ru41::util::now();
            match command {
                TodoCommand::List { view, limit } => match view {
                    Some(view) => serde_json::to_value(service.list(
                        view.into_domain(),
                        now,
                        limit as usize,
                    )?)?,
                    None => serde_json::to_value(service.overview(now, limit as usize)?)?,
                },
                TodoCommand::Add {
                    title,
                    due,
                    remind,
                    labels,
                    priority,
                } => {
                    let due_at = due
                        .as_deref()
                        .map(|text| parse_due(text, now, &options))
                        .transpose()?;
                    let explicit_remind_at = remind
                        .as_deref()
                        .map(|text| parse_due(text, now, &options))
                        .transpose()?;
                    let remind_at = explicit_remind_at.or(due_at);
                    serde_json::to_value(service.add(
                        TodoInput {
                            title,
                            priority: priority.into_domain(),
                            labels,
                            due_at,
                            remind_at,
                            source: Some(ru41::domain::todo::TodoSource {
                                channel: "cli".into(),
                                source_id: None,
                            }),
                        },
                        now,
                    )?)?
                }
                TodoCommand::Done { id } => serde_json::to_value(service.complete(&id, now)?)?,
                TodoCommand::Postpone { id, until } => {
                    serde_json::to_value(service.postpone(&id, &until, now)?)?
                }
                TodoCommand::Cancel { id } => serde_json::to_value(service.cancel(&id, now)?)?,
                TodoCommand::Restore { id } => serde_json::to_value(service.restore(&id, now)?)?,
                TodoCommand::Remind {
                    once,
                    interval_secs,
                    channel,
                    target,
                } => {
                    if once {
                        let route = todo_notify_route(&config, channel, target)?;
                        let notifier = notifier_for_route(&config, &route)?;
                        let report = ru41::app::todo::send_due_reminders(
                            &service,
                            notifier.as_ref(),
                            &route,
                            options,
                        )
                        .await?;
                        let events = ScheduleService::new(service.store().clone(), options);
                        ru41::app::schedule::send_event_reminders(
                            &events,
                            notifier.as_ref(),
                            &route,
                            options,
                        )
                        .await?;
                        println!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        run_todo_remind_loop(&config, &service, interval_secs, channel, target)
                            .await?;
                    }
                    return Ok(());
                }
            }
        }
        Command::Event { command } => {
            let options = config.todo.options()?;
            let service = ScheduleService::new(open_store(&store_config)?, options);
            let now = ru41::util::now();
            match command {
                EventCommand::Add {
                    when,
                    title,
                    location,
                } => {
                    let (starts_at, ends_at, recurrence) =
                        parse_event_when(&when, now, &options)
                            .with_context(|| format!("无法解析时间：{when}"))?;
                    serde_json::to_value(service.add(
                        EventInput {
                            title,
                            starts_at,
                            ends_at,
                            location,
                            recurrence,
                            source: Some(EventSource {
                                channel: "cli".into(),
                                source_id: None,
                            }),
                        },
                        now,
                    )?)?
                }
                EventCommand::List { days } => {
                    serde_json::to_value(service.upcoming(now, days, 100)?)?
                }
                EventCommand::Cancel { id } => serde_json::to_value(service.cancel(&id, now)?)?,
                EventCommand::Conflicts { when } => {
                    let (starts_at, ends_at, _) = parse_event_when(&when, now, &options)
                        .with_context(|| format!("无法解析时间：{when}"))?;
                    serde_json::to_value(service.conflicts(starts_at, ends_at, now)?)?
                }
            }
        }
        Command::Schedule { command } => match command {
            ScheduleCommand::List => serde_json::to_value(scheduled_jobs(&config))?,
            ScheduleCommand::Run {
                job,
                interval_secs,
                channel,
                target,
            } => {
                let store = open_store(&store_config)?;
                if job == "todo-remind" {
                    let options = config.todo.options()?;
                    let service = TodoService::new(store, options);
                    run_todo_remind_loop(
                        &config,
                        &service,
                        interval_secs.unwrap_or(60),
                        channel,
                        target,
                    )
                    .await?;
                    return Ok(());
                }
                let Some(tracker) = tracker_job(&config, store, &job)? else {
                    anyhow::bail!("unknown scheduled job: {job} (see `ru41 schedule list`)");
                };
                let interval = interval_secs
                    .map(Duration::from_secs)
                    .unwrap_or_else(|| tracker.job_interval());
                ru41::ingress::schedule::run_periodic(
                    &job,
                    interval,
                    || tracker.poll_json(),
                    shutdown_channel()?,
                )
                .await?;
                return Ok(());
            }
        },
        Command::Knowledge { command } => {
            let knowledge = build_knowledge(&config, &store_config.1)?;
            match command {
                KnowledgeCommand::Index { vault } => {
                    let vault = vault
                        .or_else(|| config.obsidian.vault_path.as_ref().map(PathBuf::from))
                        .context("vault path required: --vault 或 [obsidian].vault_path")?;
                    serde_json::to_value(
                        tokio::task::spawn_blocking(move || knowledge.index_vault(&vault))
                            .await??,
                    )?
                }
                KnowledgeCommand::Search { query, limit } => serde_json::to_value(
                    tokio::task::spawn_blocking(move || knowledge.search(&query, limit)).await??,
                )?,
            }
        }
    };
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// 知识库服务：embedding 模型缓存与向量库都落在 state.db 旁边的衍生文件。
fn build_knowledge(
    config: &Config,
    state_db: &std::path::Path,
) -> Result<ru41::app::knowledge::KnowledgeService> {
    let (db, cache) = config.knowledge.resolved(state_db);
    let embedder = Arc::new(ru41::adapters::embedding::fastembed::FastembedEmbedder::new(cache)?);
    ru41::app::knowledge::KnowledgeService::new(embedder, db)
}

async fn run_tracker_periodic(
    name: &str,
    job: &dyn ru41::ingress::schedule::PollJob,
) -> Result<()> {
    ru41::ingress::schedule::run_periodic(
        name,
        job.job_interval(),
        || job.poll_json(),
        shutdown_channel()?,
    )
    .await
}

/// 附属追踪器的统一 CLI 执行：Poll 跑一轮并返回报告，Run 常驻至收到退出信号。
/// Run 分支在退出信号后已无事可做，直接以 0 退出，避免调用方把「正常关停」
/// 当成缺报告的错误（tracker_or_exit 只服务于 Poll 分支）。
async fn tracker_cli(
    name: &str,
    command: TrackerCommand,
    config: &Config,
    store_options: &ru41::bootstrap::store::StoreOptions,
) -> Result<Option<serde_json::Value>> {
    let job = tracker_job(config, open_store(store_options)?, name)?
        .with_context(|| format!("unknown tracker job: {name}"))?;
    match command {
        TrackerCommand::Poll => Ok(Some(job.poll_json().await?)),
        TrackerCommand::Run => {
            run_tracker_periodic(name, job.as_ref()).await?;
            std::process::exit(0);
        }
    }
}

/// Run 常驻至退出信号，没有可打印的报告；此时 main 直接结束。
fn tracker_or_exit(report: Option<serde_json::Value>) -> Result<serde_json::Value> {
    report.context("tracker run exited")
}

async fn run_todo_remind_loop(
    config: &Config,
    service: &TodoService,
    interval_secs: u64,
    channel: String,
    target: Option<String>,
) -> Result<()> {
    let options = config.todo.options()?;
    let route = todo_notify_route(config, channel, target)?;
    let notifier = notifier_for_route(config, &route)?;

    // 启动补发：进程停机期间错过的课程提醒，合并成一条消息发出。
    {
        let events = ScheduleService::new(service.store().clone(), options);
        match ru41::app::schedule::send_missed_class_reminders(
            &events,
            notifier.as_ref(),
            &route,
            options,
        )
        .await
        {
            Ok((count, sent)) if sent => {
                println!(
                    "{}",
                    serde_json::to_string(&serde_json::json!({"missed_classes": count}))?
                );
            }
            Ok(_) => {}
            Err(error) => eprintln!("[todo-remind] missed-class catch-up failed: {error:#}"),
        }
    }

    // 到点提醒循环：只发 todo 到点。课程提醒走独立的 class-remind 任务（schedule run class-remind）。
    let service_ref = service;
    let notifier_ref = notifier;
    ru41::ingress::schedule::run_periodic(
        "todo-remind",
        Duration::from_secs(interval_secs),
        move || {
            let service = service_ref;
            let notifier = notifier_ref.clone();
            let route = route.clone();
            async move {
                let todo_report = ru41::app::todo::send_due_reminders(
                    service,
                    notifier.as_ref(),
                    &route,
                    options,
                )
                .await?;
                Ok(serde_json::json!({"todo": todo_report}))
            }
        },
        shutdown_channel()?,
    )
    .await
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
