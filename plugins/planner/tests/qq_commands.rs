use ru41::{
    adapters::database::sqlite::SqliteTaskStore,
    app::task::TaskService,
    channels::qq::commands::{self, Command},
    domain::{NotifyRoute, TaskStatus},
    ports::commands::{CommandContext, CommandHandler},
    ports::storage::TaskStore,
};
use ru41_plugin_planner::TodoQqCommand;
use std::sync::Arc;

#[test]
fn parses_fixed_commands() {
    for input in ["帮助", "help", "/", " / "] {
        assert_eq!(commands::parse_command(input), Some(Command::Help));
    }
    assert_eq!(commands::parse_command("/帮助"), None);
    assert_eq!(commands::parse_command("/help"), None);
    for input in ["任务", "/任务", "tasks", "/tasks"] {
        assert_eq!(commands::parse_command(input), Some(Command::List));
    }
    for input in ["任务 3", "/任务 3", "task 3", "/task 3", "show 3"] {
        assert_eq!(commands::parse_command(input), Some(Command::Show(3)));
    }
    for input in ["取消 2", "/取消 2", "cancel 2", "/cancel 2"] {
        assert_eq!(commands::parse_command(input), Some(Command::Cancel(2)));
    }
    for input in ["绑定 abc123", "/绑定 abc123", "bind abc123", "/bind abc123"] {
        assert_eq!(
            commands::parse_command(input),
            Some(Command::Bind("abc123".into()))
        );
    }
    for input in ["新建", "/新建", "clear", "/clear"] {
        assert_eq!(commands::parse_command(input), Some(Command::Clear));
    }
}

#[test]
fn natural_language_falls_through_to_agent() {
    assert_eq!(commands::parse_command("任务怎么做"), None);
    assert_eq!(commands::parse_command("任务 abc"), None);
    assert_eq!(commands::parse_command("取消"), None);
    assert_eq!(commands::parse_command("绑定"), None);
    assert_eq!(commands::parse_command("帮我总结一下今天的收件箱"), None);
}

struct Fixture {
    store: Arc<dyn TaskStore>,
    group_task: i64,
    c2c_task: i64,
    _dir: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let service = TaskService::new(store.clone());
    let group_task = service
        .enqueue_with_notify(
            "qq:msg:g1",
            "群里的分析请求",
            "codex",
            &NotifyRoute {
                channel: "qq".into(),
                target: "group:G1".into(),
                msg_id: Some("m1".into()),
            },
        )
        .unwrap()
        .task
        .id;
    let c2c_task = service
        .enqueue_with_notify(
            "qq:msg:u1",
            "单聊的分析请求",
            "codex",
            &NotifyRoute {
                channel: "qq".into(),
                target: "user:U1".into(),
                msg_id: Some("m2".into()),
            },
        )
        .unwrap()
        .task
        .id;
    Fixture {
        store,
        group_task,
        c2c_task,
        _dir: dir,
    }
}

fn ctx<'a>(
    fixture: &'a Fixture,
    target: &'a str,
    author: &'a str,
    is_c2c: bool,
) -> CommandContext<'a> {
    CommandContext {
        store: &fixture.store,
        bind_secret: Some("s3cret"),
        target,
        msg_id: "command-msg",
        author_openid: author,
        is_c2c,
        reply_channel: "qq",
        image_url: None,
        attachments: &[],
    }
}

#[test]
fn list_scopes_to_current_conversation() {
    let fixture = fixture();
    let group_view = commands::execute(
        &Command::List,
        &ctx(&fixture, "group:G1", "U9", false),
        &ru41::app::usage_report::Pricing::builtin(),
    )
    .unwrap();
    assert!(
        group_view
            .text
            .contains(&format!("#{}", fixture.group_task))
    );
    assert!(!group_view.text.contains(&format!("#{}", fixture.c2c_task)));
    // 任务列表带「详情/取消」按钮行，指令指向对应任务。
    let markup = group_view.markup.expect("task list must carry buttons");
    let first = &markup.rows[0];
    assert!(
        first[0]
            .command
            .contains(&format!("/任务 {}", fixture.group_task))
    );
    assert!(
        first[1]
            .command
            .contains(&format!("/取消 {}", fixture.group_task))
    );
    let c2c_view = commands::execute(
        &Command::List,
        &ctx(&fixture, "user:U1", "U1", true),
        &ru41::app::usage_report::Pricing::builtin(),
    )
    .unwrap();
    assert!(!c2c_view.text.contains(&format!("#{}", fixture.group_task)));
    assert!(c2c_view.text.contains(&format!("#{}", fixture.c2c_task)));
}

#[test]
fn clear_resets_only_the_current_conversation() {
    let fixture = fixture();
    fixture
        .store
        .put_document(
            "conversation",
            "qq:user:U1",
            None,
            &serde_json::json!({"backend_id": "codex", "session_id": "thread-1"}),
        )
        .unwrap();
    let reply = commands::execute(
        &Command::Clear,
        &ctx(&fixture, "user:U1", "U1", true),
        &ru41::app::usage_report::Pricing::builtin(),
    )
    .unwrap();
    assert!(reply.text.contains("新对话"));
    let document = fixture
        .store
        .document("conversation", "qq:user:U1")
        .unwrap()
        .unwrap();
    assert!(document.value["session_id"].is_null());
}

#[test]
fn bound_user_aggregates_across_conversations_in_c2c() {
    let fixture = fixture();
    let bound = commands::execute(
        &Command::Bind("s3cret".into()),
        &ctx(&fixture, "user:U1", "U1", true),
        &ru41::app::usage_report::Pricing::builtin(),
    )
    .unwrap();
    assert!(bound.text.contains("绑定成功"));
    let view = commands::execute(
        &Command::List,
        &ctx(&fixture, "user:U1", "U1", true),
        &ru41::app::usage_report::Pricing::builtin(),
    )
    .unwrap();
    assert!(view.text.contains(&format!("#{}", fixture.group_task)));
    assert!(view.text.contains(&format!("#{}", fixture.c2c_task)));
    // 群聊中即使已绑定也不聚合，私人内容不进群。
    let group_view = commands::execute(
        &Command::List,
        &ctx(&fixture, "group:G1", "U1", false),
        &ru41::app::usage_report::Pricing::builtin(),
    )
    .unwrap();
    assert!(!group_view.text.contains(&format!("#{}", fixture.c2c_task)));
}

#[tokio::test]
async fn todo_commands_add_list_and_complete_in_private_chat() {
    let fixture = fixture();
    let command = TodoQqCommand::new(ru41::domain::todo::TodoOptions::default(), None);
    let context = ctx(&fixture, "user:U1", "U1", true);

    let unbound = command
        .handle_message("#", &context)
        .await
        .unwrap()
        .unwrap();
    assert!(unbound.text.contains("身份绑定"));
    commands::execute(
        &Command::Bind("s3cret".into()),
        &context,
        &ru41::app::usage_report::Pricing::builtin(),
    )
    .unwrap();

    let added = command
        .handle_message("#明天下午3点 写项目周报", &context)
        .await
        .unwrap()
        .unwrap();
    // `#` 入口：开头时间词自动识别为截止时间；回执只给清单，不加提示行。
    assert!(!added.text.contains("已记入"), "{}", added.text);
    assert!(added.text.contains("写项目周报"), "{}", added.text);
    assert!(added.text.contains("待办（1）"), "{}", added.text);

    let documents = fixture.store.documents("todo_item", 10).unwrap();
    assert_eq!(documents.len(), 1);
    let item: ru41::domain::todo::TodoItem =
        serde_json::from_value(documents[0].value.clone()).unwrap();
    assert_eq!(item.title, "写项目周报");
    assert!(item.due_at.is_some());

    let summary = command
        .handle_message("#", &context)
        .await
        .unwrap()
        .unwrap();
    assert!(summary.text.contains("## 待办（1）"));
    assert!(summary.text.contains("写项目周报"));
    // 裸 `#` 的清单必须与 `#内容` 的清单同款：正文带中性占位符 + 一条可点的「完成」按钮。
    // 少了按钮，Telegram 侧就只剩纯符号，退化成「两跳」（先点符号再填输入框）。
    assert!(summary.text.contains("{{cmd:☐|/完成 "), "{}", summary.text);
    let summary_markup = summary.markup.expect("裸 # 的清单必须带交互按钮");
    let summary_buttons: Vec<&ru41::ports::commands::MarkupButton> =
        summary_markup.rows.iter().flatten().collect();
    assert_eq!(summary_buttons.len(), 1, "{summary_buttons:?}");
    assert!(
        summary_buttons[0].command.starts_with("/完成 "),
        "{summary_buttons:?}"
    );
    assert!(summary_buttons[0].label.contains("写项目周报"), "{summary_buttons:?}");

    let completed = command
        .handle_command("完成", item.short_id(), &context)
        .await
        .unwrap()
        .unwrap();
    // 完成回执只给清单：完成的条目落到划横线区，不带「已完成」提示行。
    assert!(!completed.text.contains("已完成"), "{}", completed.text);
    // 完成项：可点的恢复复选框（中性占位符，QQ 侧由 qq::render 展开成 cmd-input）+ 划横线标题。
    // 符号是 ▣（U+25A3）：不是 emoji 字符，跨客户端一致；
    // 不用 U+2611 ☑（emoji 字符，Telegram 上会渲染成彩色大勾框）。
    assert!(
        completed.text.contains("{{cmd:\u{25A3}|/恢复待办 "),
        "{}",
        completed.text
    );
    assert!(
        completed.text.contains("~~写项目周报~~"),
        "{}",
        completed.text
    );
    assert!(
        completed.text.contains("~~写项目周报~~"),
        "{}",
        completed.text
    );
    // 回执下方是剩余待办清单（此时已为空），不是已完成条目的详情。
    assert!(completed.text.contains("## 待办（0）"));

    let empty = command
        .handle_message("#", &context)
        .await
        .unwrap()
        .unwrap();
    assert!(empty.text.contains("## 待办（0）"));
    assert!(empty.text.contains("暂无"));
}

#[tokio::test]
async fn todo_list_rejects_group_chat() {
    let fixture = fixture();
    let command = TodoQqCommand::new(ru41::domain::todo::TodoOptions::default(), None);
    let private = ctx(&fixture, "user:U1", "U1", true);
    commands::execute(
        &Command::Bind("s3cret".into()),
        &private,
        &ru41::app::usage_report::Pricing::builtin(),
    )
    .unwrap();

    let group = command
        .handle_message("#", &ctx(&fixture, "group:G1", "U1", false))
        .await
        .unwrap()
        .unwrap();
    assert!(group.text.contains("群聊"));
}

#[tokio::test]
async fn quick_note_enters_todo_inbox_without_agent_or_schedule() {
    let fixture = fixture();
    let command = TodoQqCommand::new(ru41::domain::todo::TodoOptions::default(), None);
    let context = ctx(&fixture, "user:U1", "U1", true);

    let unbound = command
        .handle_message("#临时想法", &context)
        .await
        .unwrap()
        .unwrap();
    assert!(unbound.text.contains("身份绑定"));
    assert_eq!(fixture.store.documents("todo_item", 10).unwrap().len(), 0);

    commands::execute(
        &Command::Bind("s3cret".into()),
        &context,
        &ru41::app::usage_report::Pricing::builtin(),
    )
    .unwrap();
    let reply = command
        .handle_message("#有空整理一下英文播客订阅", &context)
        .await
        .unwrap()
        .unwrap();
    // 回执只给当前完整待办清单，不带「已记入」提示行。
    assert!(!reply.text.contains("已记入"), "{}", reply.text);
    assert!(reply.text.contains("## 待办（1）"));
    // 行首复选框：中性占位符（QQ 侧展开成行内 cmd-input，点击把 /完成 <id> 填进输入框）。
    assert!(reply.text.contains("{{cmd:☐|/完成 "));
    // 复选框符号是 ☐，与完成态的 ▣ 配成一套（两个都是非 emoji 的纯文本字形）。
    assert!(reply.text.contains("{{cmd:☐|"));

    let documents = fixture.store.documents("todo_item", 10).unwrap();
    assert_eq!(documents.len(), 1);
    let item: ru41::domain::todo::TodoItem =
        serde_json::from_value(documents[0].value.clone()).unwrap();
    assert_eq!(item.title, "有空整理一下英文播客订阅");
    assert_eq!(item.labels, vec!["临时想法"]);
    assert!(item.due_at.is_none());
    assert!(item.remind_at.is_none());

    // 非整句前缀不拦截，仍交给自然语言任务链路。
    assert!(
        command
            .handle_message("聊天记录里的 #标签 不算", &context)
            .await
            .unwrap()
            .is_none()
    );

    // 单独一个 `#`：列出全部待办。
    let list = command
        .handle_message("#", &context)
        .await
        .unwrap()
        .unwrap();
    assert!(list.text.contains("## 待办（1）"));
    assert!(list.text.contains("有空整理一下英文播客订阅"));
}

#[test]
fn extension_commands_match_aliases_and_join_help() {
    let todo = TodoQqCommand::new(ru41::domain::todo::TodoOptions::default(), None);
    for verb in ["完成", "推迟", "取消待办", "恢复待办"] {
        assert!(todo.matches(verb));
    }
    // 清单与录入统一走 `#`：/待办 与录入类命令词都不再匹配。
    for verb in ["待办", "todo", "记待办", "记一下", "想法", "note"] {
        assert!(!todo.matches(verb));
    }

    let handlers: Vec<Arc<dyn CommandHandler>> = vec![Arc::new(todo)];
    let help = commands::help_text(&handlers);
    assert!(help.contains("Ru41 · 个人 AI 助理"));
    assert!(help.contains("## 记录与日程"));
    assert!(!help.contains("## 其他能力"));
    assert!(help.contains("`#`：全部待办清单"));
    assert!(help.contains("当前对话会自动保留上下文"));
}

#[test]
fn bind_requires_configured_and_matching_secret() {
    let fixture = fixture();
    let disabled = CommandContext {
        bind_secret: None,
        ..ctx(&fixture, "user:U1", "U1", true)
    };
    let reply = commands::execute(
        &Command::Bind("s3cret".into()),
        &disabled,
        &ru41::app::usage_report::Pricing::builtin(),
    )
    .unwrap();
    assert!(reply.text.contains("未开放"));
    let wrong = commands::execute(
        &Command::Bind("nope".into()),
        &ctx(&fixture, "user:U1", "U1", true),
        &ru41::app::usage_report::Pricing::builtin(),
    )
    .unwrap();
    assert!(wrong.text.contains("口令错误"));
}

#[test]
fn show_and_cancel_reject_cross_conversation_tasks() {
    let fixture = fixture();
    let denied = commands::execute(
        &Command::Show(fixture.c2c_task),
        &ctx(&fixture, "group:G1", "U9", false),
        &ru41::app::usage_report::Pricing::builtin(),
    )
    .unwrap();
    assert!(denied.text.contains("无权"));
    let denied = commands::execute(
        &Command::Cancel(fixture.c2c_task),
        &ctx(&fixture, "group:G1", "U9", false),
        &ru41::app::usage_report::Pricing::builtin(),
    )
    .unwrap();
    assert!(denied.text.contains("无权"));
    assert_eq!(
        fixture.store.get(fixture.c2c_task).unwrap().unwrap().status,
        TaskStatus::Queued
    );
    let cancelled = commands::execute(
        &Command::Cancel(fixture.c2c_task),
        &ctx(&fixture, "user:U1", "U1", true),
        &ru41::app::usage_report::Pricing::builtin(),
    )
    .unwrap();
    assert!(cancelled.text.contains("已取消"));
    assert_eq!(
        fixture.store.get(fixture.c2c_task).unwrap().unwrap().status,
        TaskStatus::Cancelled
    );
    let detail = commands::execute(
        &Command::Show(fixture.c2c_task),
        &ctx(&fixture, "user:U1", "U1", true),
        &ru41::app::usage_report::Pricing::builtin(),
    )
    .unwrap();
    assert!(detail.text.contains("已取消"));
    assert!(detail.text.contains("单聊的分析请求"));
}

#[test]
fn c2c_new_message_interrupts_unfinished_tasks_same_conversation() {
    use ru41::channels::qq::QqBot;
    use ru41::channels::qq::api::QqApiClient;
    use ru41::channels::qq::gateway::GatewayEvent;

    let fixture = fixture();
    let bot = QqBot {
        api: Arc::new(QqApiClient::new("app", "secret")),
        store: fixture.store.clone(),
        backend: "codex".into(),
        bind_secret: None,
        command_handlers: vec![],
        image_consumers: vec![],
        sequencer: Arc::new(ru41::channels::qq::ReplySequencer::default()),
        coalesce: Arc::new(std::sync::Mutex::new(
            ru41::channels::qq::CoalesceState::default(),
        )),
        typing_sent_at: std::sync::Mutex::new(std::collections::HashMap::new()),
        pricing: ru41::app::usage_report::Pricing::builtin(),
    };
    let c2c = |msg_id: &str, content: &str| GatewayEvent::C2cMessage {
        msg_id: msg_id.into(),
        content: content.into(),
        user_openid: "U777".into(),
        image_url: None,
        attachments: vec![],
    };

    // 第一条直接入队，不发回执（用户直接看活动流）。
    let receipt = bot.ingest(&c2c("m1", "第一条")).unwrap();
    assert!(receipt.created);
    assert_eq!(receipt.interrupted, 0);
    assert!(receipt.interrupt_text().is_none());
    // 同会话第二条：打断第一条（排队中），只回打断提示。
    let receipt = bot.ingest(&c2c("m2", "第二条")).unwrap();
    assert_eq!(receipt.interrupted, 1);
    assert_eq!(
        receipt.interrupt_text().as_deref(),
        Some("⏹ 已打断上一条未完成的任务（1 个）")
    );
    // 重复消息（同 msg_id）：不重复创建任务，也不回执。
    let receipt = bot.ingest(&c2c("m2", "第二条")).unwrap();
    assert!(!receipt.created);
    assert!(receipt.interrupt_text().is_none());
    // 群消息不打断单聊任务，也不回执。
    let group = GatewayEvent::GroupMessage {
        msg_id: "g9".into(),
        content: "群消息".into(),
        group_openid: "G1".into(),
        author_openid: "U9".into(),
        attachments: vec![],
    };
    let receipt = bot.ingest(&group).unwrap();
    assert_eq!(receipt.interrupted, 0);
    assert!(receipt.interrupt_text().is_none());

    let tasks = fixture.store.list(10).unwrap();
    let status_of = |key: &str| {
        tasks
            .iter()
            .find(|t| t.request_key == key)
            .map(|t| t.status)
    };
    assert_eq!(status_of("qq:msg:m1"), Some(TaskStatus::Cancelled));
    assert_eq!(status_of("qq:msg:m2"), Some(TaskStatus::Queued));
    assert_eq!(status_of("qq:msg:g9"), Some(TaskStatus::Queued));
}

#[tokio::test]
async fn todo_list_carries_channel_neutral_action_buttons() {
    // 待办清单带渠道无关的交互按钮：QQ 侧正文里是行内 chip（不重复挂键盘），
    // 其它渠道（Telegram）把动作放底部按钮，点一下直接完成（一跳）。
    let fixture = fixture();
    let command = TodoQqCommand::new(ru41::domain::todo::TodoOptions::default(), None);
    let context = ctx(&fixture, "user:U1", "U1", true);
    commands::execute(
        &Command::Bind("s3cret".into()),
        &context,
        &ru41::app::usage_report::Pricing::builtin(),
    )
    .unwrap();
    let reply = command
        .handle_message("#写项目周报", &context)
        .await
        .unwrap()
        .unwrap();
    let markup = reply.markup.expect("待办清单应带交互按钮");
    let buttons: Vec<&ru41::ports::commands::MarkupButton> =
        markup.rows.iter().flatten().collect();
    assert!(!buttons.is_empty(), "按钮不能为空");
    assert!(
        buttons.iter().any(|b| b.command.starts_with("/完成 ")),
        "应有「完成 <id>」动作：{buttons:?}"
    );
    // 正文里是中性占位符（平台专有标签由网关展开）。
    assert!(reply.text.contains("{{cmd:☐|/完成 "), "{}", reply.text);
}

/// 按钮必须与正文一一对应：正文只显示最近 5 条已完成，按钮却给全部历史条目
/// 都配过（正文里没有的动作反而有按钮）—— Telegram 侧靠这个不变量判断
/// 「底部按钮是不是重复入口」。
#[tokio::test]
async fn todo_buttons_cover_exactly_the_items_shown_in_text() {
    let fixture = fixture();
    let command = TodoQqCommand::new(ru41::domain::todo::TodoOptions::default(), None);
    let context = ctx(&fixture, "user:U1", "U1", true);
    commands::execute(
        &Command::Bind("s3cret".into()),
        &context,
        &ru41::app::usage_report::Pricing::builtin(),
    )
    .unwrap();

    // 8 条已完成（正文只显示最近 5 条）+ 2 条未完成。
    for index in 0..8 {
        command
            .handle_message(&format!("#已完成{index}"), &context)
            .await
            .unwrap()
            .unwrap();
    }
    let documents = fixture.store.documents("todo_item", 10).unwrap();
    for document in &documents {
        let item: ru41::domain::todo::TodoItem =
            serde_json::from_value(document.value.clone()).unwrap();
        command
            .handle_command("完成", item.short_id(), &context)
            .await
            .unwrap()
            .unwrap();
    }
    for index in 0..2 {
        command
            .handle_message(&format!("#未完成{index}"), &context)
            .await
            .unwrap()
            .unwrap();
    }

    let list = command.handle_message("#", &context).await.unwrap().unwrap();
    let tokens = ru41::interactive::cmd_tokens(&list.text);
    let markup = list.markup.expect("待办清单应带交互按钮");
    let buttons: Vec<&ru41::ports::commands::MarkupButton> =
        markup.rows.iter().flatten().collect();

    // 正文里出现的可点指令 = 2 条未完成 + 5 条已完成（只显示最近 5 条）。
    assert_eq!(tokens.len(), 7, "{}", list.text);
    // 按钮与正文一一对应。
    assert_eq!(buttons.len(), tokens.len(), "按钮与正文对不上");
    for button in &buttons {
        assert!(
            tokens.iter().any(|(_, _, command)| command == &button.command),
            "按钮动作不在正文里：{button:?}"
        );
    }
}
