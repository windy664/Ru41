use anyhow::{Context, Result};
use clap::Parser;
use ru41::bootstrap::agents::{AgentOverrides, registry};
use ru41::bootstrap::assembly::{
    default_notify_route, knowledge_service, notifier_for_route, notify_channels, qq_api,
    qq_command_handlers, qq_image_consumers, scheduled_jobs, todo_notify_route, tracker_job,
};
use ru41::bootstrap::store::{open_store, resolve as resolve_store};
use ru41::{
    app::{schedule::ScheduleService, task::TaskService, todo::TodoService},
    bootstrap::config::Config,
    channels::qq::{CoalesceState, QqBot, ReplySequencer},
    domain::schedule::{EventInput, EventSource, parse_event_when},
    domain::todo::{TodoInput, parse_due},
    ingress::cli::{
        Cli, Command, EventCommand, KnowledgeCommand, QqCommand, ScheduleCommand, SecretCommand,
        TaskCommand, TodoCommand, TrackerCommand,
    },
    ingress::cli_ops,
    privacy::{NoopSanitizer, RandomSanitizer, Sanitizer},
    runtime::dispatcher::NotifyDispatcher,
    runtime::worker::{Worker, WorkerConfig},
};
use std::collections::HashMap;
use std::sync::Mutex;
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
    if let Some(path) = &config_path
        && std::env::var("RU41_VERBOSE").is_ok()
    {
        eprintln!("[config] using {}", path.display());
    }
    // 动态插件（cdylib）：启动时扫描加载，失败只告警不挡启动。
    if config.plugins.is_enabled() {
        let dir = config.plugins.dir_or_default();
        match ru41::plugins::load_all(&dir) {
            Ok(n) if n > 0 => eprintln!("[plugins] {n} 个外部插件已注册"),
            Ok(_) => {}
            Err(error) => eprintln!("[plugins] 插件目录 {} 加载失败: {error:#}", dir.display()),
        }
    }
    // 插件加载完成后再做 [sources] 未知键告警（否则插件的配置段全被误报）。
    ru41::bootstrap::config::warn_unknown_source_keys(&config.sources);
    // 附属通用入口（ru41 <模块名> poll|run|<专属子命令>）：模块名经注册表校验；
    // poll|run 统一走 tracker 分发，其余子命令交给模块自己的 cli 钩子（如 youtube prep）。
    if let Command::Tracker(args) = &cli.command {
        let name = args
            .first()
            .context("用法: ru41 <模块名> poll|run（模块名见 schedule list）")?;
        let defs = ru41::modules::sorted();
        let def = defs
            .iter()
            .find(|def| def.name == name.as_str())
            .with_context(|| format!("未知模块: {name}（可用模块见 schedule list）"))?;
        match args.get(1).map(String::as_str) {
            Some("poll") | Some("run") => {
                let tracker_command = if args[1] == "poll" {
                    TrackerCommand::Poll
                } else {
                    TrackerCommand::Run
                };
                let output = tracker_or_exit(
                    tracker_cli(name, tracker_command, &config, &store_config).await?,
                )?;
                println!("{}", serde_json::to_string_pretty(&output)?);
            }
            _ => {
                let output = (def.cli)(&config, open_store(&store_config)?, &args[1..])
                    .await?
                    .with_context(|| format!("用法: ru41 {name} poll|run（或该模块专属子命令）"))?;
                println!("{}", serde_json::to_string_pretty(&output)?);
            }
        }
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
            let state = ru41::ingress::http::ApiState::with_default_backend(
                open_store(&store_config)?,
                Arc::new(registry(&config.agent, AgentOverrides::default())?),
                token,
                config.agent.default_backend().to_owned(),
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
        | Command::Vault { .. }
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
                silent_sources: config.notify.silent_sources.clone().unwrap_or_default(),
                config: WorkerConfig {
                    worker_id: format!("worker-{}", std::process::id()),
                    runs_dir: runs_dir
                        .or_else(|| config.worker.runs_dir.clone())
                        .unwrap_or_else(|| Config::default_data_dir().join("runs")),
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
                // 聊天走带会话的底座变体（pi-session）：同一会话的多轮消息才能续接上下文；
                // 命令处理器仍用无会话的底座，插件的一次性任务不必落会话文件。
                let base_backend =
                    backend.unwrap_or_else(|| config.agent.default_backend().to_owned());
                let backend = ru41::bootstrap::agents::chat_backend(
                    &config.agent,
                    Some(base_backend.clone()),
                );
                let bind_secret = bind_secret.or_else(|| config.channel.qq.bind_secret.clone());
                let api = Arc::new(qq_api(&credentials, &config)?);
                let store = open_store(&store_config)?;
                let command_handlers = qq_command_handlers(&config, &base_backend)?;
                let image_consumers = qq_image_consumers(&config)?;
                // 回执与终态/进度通知共享序号器，同 msg_id 被动回复 seq 不撞车。
                let sequencer = Arc::new(ReplySequencer::default());
                let pricing = ru41::app::usage_report::Pricing::from_config(&config.pricing);
                let bot = Arc::new(QqBot {
                    api: api.clone(),
                    store: store.clone(),
                    backend,
                    bind_secret,
                    command_handlers,
                    image_consumers,
                    sequencer: sequencer.clone(),
                    typing_sent_at: Mutex::new(HashMap::new()),
                    coalesce: Arc::new(Mutex::new(CoalesceState::default())),
                    pricing,
                });
                let dispatcher = NotifyDispatcher {
                    store,
                    channels: notify_channels(&config, Some(api), Some(sequencer))?,
                    batch: 20,
                    interval: Duration::from_secs(2),
                    silent_sources: config.notify.silent_sources.clone().unwrap_or_default(),
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
                user_openid,
                text,
                media,
                kind,
            } => {
                let api = qq_api(&credentials, &config)?;
                let value = match (group_openid, user_openid, media) {
                    (Some(group), None, media) => {
                        qq_send(&api, SendTarget::Group(&group), &text, media, kind).await?
                    }
                    (None, Some(user), media) => {
                        qq_send(&api, SendTarget::User(&user), &text, media, kind).await?
                    }
                    _ => anyhow::bail!("--group-openid 与 --user-openid 必须二选一"),
                };
                serde_json::to_value(value)?
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
                silent_sources: config.notify.silent_sources.clone().unwrap_or_default(),
            };
            dispatcher.run_loop(shutdown_channel()?).await?;
            return Ok(());
        }
        Command::Tracker(_) => unreachable!("handled by tracker dispatch above"),
        Command::Task { command } => {
            let mut service = TaskService::new(open_store(&store_config)?);
            match command {
                TaskCommand::Add {
                    key,
                    prompt,
                    backend,
                    notify,
                } => {
                    let backend =
                        backend.unwrap_or_else(|| config.agent.default_backend().to_owned());
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
                TaskCommand::Trace { id } => {
                    // 汇总最近一次尝试的底座事件：工具、技能包、改动文件、命令。
                    let attempts = service.attempts(id)?;
                    let mut summary = ru41::app::agent_trace::TraceSummary::default();
                    let mut scanned = 0usize;
                    for attempt in attempts.iter().rev() {
                        let Some(run_dir) = attempt.run_dir.as_deref() else {
                            continue;
                        };
                        let path = std::path::Path::new(run_dir).join("events.jsonl");
                        if !path.exists() {
                            continue;
                        }
                        summary = ru41::app::agent_trace::summarize_events(&path)?;
                        scanned += 1;
                        break;
                    }
                    serde_json::json!({
                        "task_id": id,
                        "attempts_scanned": scanned,
                        "tools": summary.tools,
                        "skills": summary.skills,
                        "changed_files": summary.changed_files,
                        "messages": summary.messages,
                        "commands": summary.commands,
                    })
                }
                TaskCommand::Cancel { id } => serde_json::to_value(service.cancel(id)?)?,
                TaskCommand::Retry { id } => serde_json::to_value(service.retry(id)?)?,
                TaskCommand::Attempts { id } => serde_json::to_value(service.attempts(id)?)?,
                TaskCommand::Usage { days, out } => {
                    let store = open_store(&store_config)?;
                    let now = ru41::util::now();
                    let since = now - i64::from(days) * 86_400;
                    let week = ru41::app::usage::overview(&store.usage_runs_since(since)?, days);
                    if let Some(path) = out {
                        let day =
                            ru41::app::usage::overview(&store.usage_runs_since(now - 86_400)?, 1);
                        let report = ru41::app::usage_report::render(
                            &week,
                            &day,
                            ru41::app::usage_report::Style::Doc,
                            &ru41::app::usage_report::Pricing::from_config(&config.pricing),
                        );
                        if let Some(parent) = std::path::Path::new(&path).parent() {
                            std::fs::create_dir_all(parent)?;
                        }
                        std::fs::write(&path, report)?;
                        serde_json::json!({ "written": path })
                    } else {
                        serde_json::json!({
                            "window_days": days,
                            "runs": week.runs,
                            "reported_runs": week.reported_runs,
                            "input_tokens": week.input_tokens,
                            "cached_input_tokens": week.cached_input_tokens,
                            "output_tokens": week.output_tokens,
                            "total_tokens": week.total_tokens,
                            "by_model": week.by_model,
                            "by_source": week.by_source,
                            "by_day": week.by_day,
                        })
                    }
                }
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
        // 凭据托管：get 把真值原样写 stdout（给 shell `$(ru41 secret get <名>)`
        // 内联展开用，真值不进模型上下文），list 只列名称。记录墓碑
        //（deleted=true）当作不存在。
        Command::Secret { command } => {
            use ru41::domain::credential as cred;
            let store = open_store(&store_config)?;
            let live = |doc: &ru41::domain::records::Document| {
                doc.value["deleted"].as_bool() != Some(true)
            };
            // 寻址：按标识或唯一标签（共享实现，标签重复宁可报错也不猜）。
            let resolve = |selector: &str| cred::resolve(store.as_ref(), selector);
            match command {
                SecretCommand::Exec { id, command } => {
                    if command.is_empty() {
                        anyhow::bail!("用法：ru41 secret exec --id <标识> -- <命令>");
                    }
                    // 多条凭据一起注入：FTP 这类要账号+密码的场景不用跑两次。
                    let mut docs = Vec::new();
                    for selector in &id {
                        let Some(doc) = resolve(selector)? else {
                            anyhow::bail!("凭据不存在或已删除：{selector}");
                        };
                        docs.push(doc);
                    }
                    // 保留参数原样执行：命令本来就是 shell 调用（sh -c '...'）时，
                    // 拼字符串会丢掉引号；不是 shell 调用时用 sh -c 走一遍拿回
                    // 管道/重定向/变量展开。
                    let mut cmd = match command.first().map(String::as_str) {
                        Some("sh") | Some("bash") | Some("zsh") => {
                            let mut cmd = tokio::process::Command::new(&command[0]);
                            cmd.args(&command[1..]);
                            cmd
                        }
                        _ => {
                            let mut cmd = tokio::process::Command::new("sh");
                            cmd.arg("-c").arg(command.join(" "));
                            cmd
                        }
                    };
                    let mut ids = Vec::new();
                    for (index, doc) in docs.iter().enumerate() {
                        let value = doc.value["value"].as_str().unwrap_or_default().to_owned();
                        if value.is_empty() {
                            anyhow::bail!("凭据为空：{}", doc.id);
                        }
                        // 第一条进 RU41_SECRET，其余进 RU41_SECRET_2/3…
                        let var = if index == 0 {
                            "RU41_SECRET".to_owned()
                        } else {
                            format!("RU41_SECRET_{}", index + 1)
                        };
                        cmd.env(&var, &value);
                        ids.push(doc.id.clone());
                    }
                    cmd.env("RU41_SECRET_ID", ids.first().cloned().unwrap_or_default());
                    cmd.env("RU41_SECRET_IDS", ids.join(","));
                    let output = cmd.output().await.context("执行命令失败")?;
                    // stdout/stderr 里的真值一律掩码——模型看到的不是凭据本身。
                    let mask = "••••••••";
                    let scrub = |text: &str| -> String {
                        let mut out = text.to_owned();
                        for doc in &docs {
                            if let Some(value) = doc.value["value"].as_str() {
                                out = out.replace(value, mask);
                            }
                        }
                        out
                    };
                    let stdout = scrub(&String::from_utf8_lossy(&output.stdout));
                    let stderr = scrub(&String::from_utf8_lossy(&output.stderr));
                    // 审计：只记标识与命令（命令里的真值同样掩码）。
                    let audit_id = format!("{}-{}", ru41::util::now(), ids.join("_"));
                    let _ = store.put_document(
                        "credential_audit",
                        &audit_id,
                        None,
                        &serde_json::json!({
                            "credential_ids": ids,
                            "command": scrub(&command.join(" ")).chars().take(200).collect::<String>(),
                            "at": ru41::util::now(),
                            "exit_code": output.status.code(),
                        }),
                    );
                    print!("{stdout}");
                    eprint!("{stderr}");
                    let code = output.status.code().unwrap_or(1);
                    std::process::exit(code);
                }
                SecretCommand::Get { id, allow_plain } => {
                    // 明文会进模型上下文：默认拒绝，agent 应该用 secret exec。
                    if !allow_plain {
                        anyhow::bail!(
                            "拒绝直接输出明文：明文进上下文即视为泄露。请用 \
                             `ru41 secret exec --id {id} -- <命令>`（真值注入环境变量、输出掩码），\
                             人工确需读取再加 --allow-plain"
                        );
                    }
                    let Some(doc) = resolve(&id)? else {
                        anyhow::bail!("凭据不存在或已删除：{id}");
                    };
                    let value = doc.value["value"].as_str().unwrap_or_default().to_owned();
                    println!("{value}");
                }
                SecretCommand::List => {
                    let mut rows: Vec<(String, String)> = store
                        .documents(cred::KIND, 500)?
                        .into_iter()
                        .filter(live)
                        .map(|doc| {
                            (
                                doc.id,
                                doc.value["label"].as_str().unwrap_or_default().to_owned(),
                            )
                        })
                        .collect();
                    rows.sort();
                    for (id, label) in rows {
                        println!("{id}\t{label}");
                    }
                }
                SecretCommand::Set { label, value } => {
                    let id = cred::new_id();
                    store.put_document(
                        cred::KIND,
                        &id,
                        None,
                        &serde_json::json!({
                            "value": value,
                            "label": label,
                            "updated_at": ru41::util::now(),
                        }),
                    )?;
                    println!("{id}");
                }
                SecretCommand::Delete { id } => {
                    let Some(doc) = resolve(&id)? else {
                        anyhow::bail!("凭据不存在或已删除：{id}");
                    };
                    store.put_document(
                        cred::KIND,
                        &doc.id,
                        Some(doc.revision),
                        &serde_json::json!({
                            "deleted": true,
                            "label": doc.value["label"].clone(),
                            "updated_at": ru41::util::now(),
                        }),
                    )?;
                    println!("deleted: {}", doc.id);
                }
                SecretCommand::Audit { limit } => {
                    let mut rows: Vec<(String, String, String)> = store
                        .documents("credential_audit", 500)?
                        .into_iter()
                        .map(|doc| {
                            (
                                doc.value["at"]
                                    .as_i64()
                                    .map(|at| at.to_string())
                                    .unwrap_or_default(),
                                doc.value["credential_ids"]
                                    .as_array()
                                    .map(|ids| {
                                        ids.iter()
                                            .filter_map(|v| v.as_str())
                                            .collect::<Vec<_>>()
                                            .join(",")
                                    })
                                    .unwrap_or_default(),
                                doc.value["command"].as_str().unwrap_or_default().to_owned(),
                            )
                        })
                        .collect();
                    rows.sort();
                    for (at, credential_id, command) in rows.into_iter().rev().take(limit as usize)
                    {
                        println!("{at}\t{credential_id}\t{command}");
                    }
                }
            }
            return Ok(());
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

/// `qq send` 的目标端。
enum SendTarget<'a> {
    Group(&'a str),
    User(&'a str),
}

/// 文本或富媒体发送；媒体类型显式指定优先，否则按扩展名猜，猜不中按文件。
async fn qq_send(
    api: &ru41::channels::qq::api::QqApiClient,
    target: SendTarget<'_>,
    text: &str,
    media: Option<String>,
    kind: Option<String>,
) -> Result<serde_json::Value> {
    use ru41::ports::channel::{MediaKind, MediaRef};
    if let Some(source) = media {
        let kind = match kind.as_deref().map(str::to_lowercase).as_deref() {
            Some("image") => MediaKind::Image,
            Some("video") => MediaKind::Video,
            Some("voice") => MediaKind::Voice,
            Some("file") => MediaKind::File,
            Some(other) => anyhow::bail!("未知媒体类型 {other}（可选 image|video|voice|file）"),
            None => guess_media_kind(&source),
        };
        let media = MediaRef::new(kind, source);
        return Ok(match target {
            SendTarget::Group(group) => api.send_group_media(group, text, &media, None, 1).await?,
            SendTarget::User(user) => api.send_c2c_media(user, text, &media, None, 1).await?,
        });
    }
    anyhow::ensure!(!text.trim().is_empty(), "文本与 --media 至少给一样");
    Ok(match target {
        SendTarget::Group(group) => api.send_group_text(group, text, None, 1).await?,
        SendTarget::User(user) => api.send_c2c_text(user, text, None, 1).await?,
    })
}

fn guess_media_kind(source: &str) -> ru41::ports::channel::MediaKind {
    use ru41::ports::channel::MediaKind;
    let ext = source
        .rsplit(['/', '.'])
        .next()
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" => MediaKind::Image,
        "mp4" | "mov" | "avi" | "mkv" | "webm" => MediaKind::Video,
        "silk" | "amr" | "mp3" | "wav" | "m4a" | "ogg" => MediaKind::Voice,
        _ => MediaKind::File,
    }
}
