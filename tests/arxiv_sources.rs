use ru41::{
    adapters::database::sqlite::SqliteTaskStore,
    domain::NotifyRoute,
    modules::papers::{
        PaperTracker, PapersConfig, PapersQqCommand, deep_read_task,
        source::{ArxivClient, parse_feed},
    },
    ports::commands::{CommandContext, CommandHandler},
    ports::storage::TaskStore,
};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    sync::Arc,
};

const SAMPLE_FEED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>arXiv query</title>
  <entry>
    <id>http://arxiv.org/abs/2510.11111v2</id>
    <title>  Prompt Injection Attacks on LLM Agents  </title>
    <summary>We study prompt injection.
Multi-line abstract &amp; entities.</summary>
    <author><name>Alice Zhang</name></author>
    <author><name>Bob Li</name></author>
    <published>2026-10-13T01:00:00Z</published>
    <updated>2026-10-14T02:00:00Z</updated>
    <link rel="alternate" href="http://arxiv.org/abs/2510.11111v2"/>
    <link title="pdf" href="http://arxiv.org/pdf/2510.11111v2"/>
  </entry>
  <entry>
    <id>http://arxiv.org/abs/2510.22222v1</id>
    <title>Elliptic Curve Pairings Revisited</title>
    <summary>Pure cryptography, no machine learning.</summary>
    <author><name>Carol Wang</name></author>
    <published>2026-10-13T03:00:00Z</published>
    <updated>2026-10-13T03:00:00Z</updated>
    <link rel="alternate" href="http://arxiv.org/abs/2510.22222v1"/>
  </entry>
</feed>"#;

#[test]
fn parses_atom_feed_entries() {
    let entries = parse_feed(SAMPLE_FEED).unwrap();
    assert_eq!(entries.len(), 2);
    let first = &entries[0];
    assert_eq!(first.id, "2510.11111");
    assert_eq!(first.version, 2);
    assert_eq!(first.title, "Prompt Injection Attacks on LLM Agents");
    assert!(first.summary.contains("Multi-line abstract & entities."));
    assert_eq!(first.authors, vec!["Alice Zhang", "Bob Li"]);
    assert_eq!(first.abs_url, "http://arxiv.org/abs/2510.11111v2");
    assert_eq!(
        first.pdf_url.as_deref(),
        Some("http://arxiv.org/pdf/2510.11111v2")
    );
    let second = &entries[1];
    assert_eq!(second.id, "2510.22222");
    assert_eq!(second.version, 1);
    assert_eq!(second.pdf_url, None);
}

/// 本地 mock arXiv API：任何 GET 都返回样本 feed。
fn mock_arxiv_server(payload: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut content_length = 0;
            let mut line = String::new();
            loop {
                line.clear();
                if reader.read_line(&mut line).unwrap() == 0 {
                    break;
                }
                let lower = line.to_lowercase();
                if let Some(value) = lower.strip_prefix("content-length:") {
                    content_length = value.trim().parse().unwrap_or(0);
                }
                if line.trim().is_empty() {
                    break;
                }
            }
            let mut body = vec![0; content_length];
            reader.read_exact(&mut body).unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/atom+xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                payload.len(),
                payload
            )
            .unwrap();
        }
    });
    base
}

fn make_tracker(store: &Arc<dyn TaskStore>, base_url: &str, keywords: Vec<String>) -> PaperTracker {
    PaperTracker::new(
        store.clone(),
        ArxivClient::with_base_url(base_url),
        PapersConfig {
            categories: vec!["cs.CR".into()],
            keywords,
            interests: "AI 安全方向本科生".into(),
            max_results: 50,
            interval_secs: 21_600,
            backend: "codex".into(),
            notify: Some(NotifyRoute {
                channel: "feishu".into(),
                target: "default".into(),
                msg_id: None,
            }),
        },
    )
}

#[tokio::test]
async fn poll_enqueues_one_digest_and_dedups_seen_entries() {
    let base = mock_arxiv_server(SAMPLE_FEED);
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let tracker = make_tracker(&store, &base, vec![]);

    let first = tracker.poll_once().await.unwrap();
    assert_eq!(first.fetched, 2);
    assert_eq!(first.candidates, 2);
    let task_id = first.digest_task_id.unwrap();
    let task = store.get(task_id).unwrap().unwrap();
    assert!(task.request_key.starts_with("arxiv:digest:"));
    assert!(
        task.prompt
            .contains("Prompt Injection Attacks on LLM Agents")
    );
    assert!(task.prompt.contains("Elliptic Curve Pairings Revisited"));
    assert!(task.prompt.contains("基于摘要"));
    let notify = task.notify.unwrap();
    assert_eq!(notify.channel, "feishu");
    assert_eq!(notify.target, "default");

    // 第二轮：相同 feed 全部已见，不再入队。
    let second = tracker.poll_once().await.unwrap();
    assert_eq!(second.new_entries, 0);
    assert_eq!(second.digest_task_id, None);
    assert_eq!(store.list(10).unwrap().len(), 1);
}

#[tokio::test]
async fn keyword_filter_drops_irrelevant_entries() {
    let base = mock_arxiv_server(SAMPLE_FEED);
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let tracker = make_tracker(&store, &base, vec!["prompt injection".into()]);

    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.candidates, 1);
    let task = store.get(report.digest_task_id.unwrap()).unwrap().unwrap();
    assert!(task.prompt.contains("Prompt Injection Attacks"));
    assert!(!task.prompt.contains("Elliptic Curve"));
}

#[tokio::test]
async fn empty_candidates_create_no_task() {
    let base = mock_arxiv_server(SAMPLE_FEED);
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let tracker = make_tracker(&store, &base, vec!["no-such-keyword".into()]);

    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.candidates, 0);
    assert_eq!(report.digest_task_id, None);
    assert!(store.list(10).unwrap().is_empty());
    // 但 seen 已登记：换关键词也不会重复捞同一批。
    let tracker = make_tracker(&store, &base, vec![]);
    let second = tracker.poll_once().await.unwrap();
    assert_eq!(second.new_entries, 0);
}

const EMPTY_FEED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>arXiv query</title>
</feed>"#;

fn deep_read_route() -> NotifyRoute {
    NotifyRoute {
        channel: "qq".into(),
        target: "user:U1".into(),
        msg_id: Some("m9".into()),
    }
}

#[tokio::test]
async fn deep_read_enqueues_task_and_dedups_by_id() {
    let base = mock_arxiv_server(SAMPLE_FEED);
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let client = ArxivClient::with_base_url(&base);

    let text = deep_read_task(
        &store,
        &client,
        "2510.11111",
        "AI 安全",
        "codex",
        &deep_read_route(),
    )
    .await
    .unwrap();
    assert!(text.contains("精读任务已创建"));
    assert!(text.contains("Prompt Injection Attacks on LLM Agents"));
    let tasks = store.list(10).unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].request_key, "arxiv:deep:2510.11111");
    assert!(tasks[0].prompt.contains("基于摘要"));
    assert!(tasks[0].prompt.contains("AI 安全"));
    let notify = tasks[0].notify.clone().unwrap();
    assert_eq!(notify.channel, "qq");
    assert_eq!(notify.target, "user:U1");

    // 同一篇论文重复精读：不入队，回复已有任务编号。
    let again = deep_read_task(
        &store,
        &client,
        "2510.11111",
        "AI 安全",
        "codex",
        &deep_read_route(),
    )
    .await
    .unwrap();
    assert!(again.contains("已有精读任务"));
    assert!(again.contains(&format!("#{}", tasks[0].id)));
    assert_eq!(store.list(10).unwrap().len(), 1);
}

#[tokio::test]
async fn deep_read_unknown_id_replies_not_found() {
    let base = mock_arxiv_server(EMPTY_FEED);
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let client = ArxivClient::with_base_url(&base);

    let text = deep_read_task(
        &store,
        &client,
        "2510.99999",
        "",
        "codex",
        &deep_read_route(),
    )
    .await
    .unwrap();
    assert!(text.contains("未找到论文 2510.99999"));
    assert!(store.list(10).unwrap().is_empty());
}

#[tokio::test]
async fn qq_deep_read_handler_validates_and_executes() {
    let base = mock_arxiv_server(SAMPLE_FEED);
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let handler = PapersQqCommand::new(
        ArxivClient::with_base_url(&base),
        "AI 安全".into(),
        "codex".into(),
    );
    let ctx = CommandContext {
        store: &store,
        bind_secret: None,
        target: "user:U1",
        msg_id: "m9",
        author_openid: "U1",
        is_c2c: true,
        reply_channel: "qq",
        image_url: None,
    };

    // 缺参数与非法 ID 不拦截，交回 QQ 落入普通 Agent 消息。
    assert!(handler.handle("", &ctx).await.unwrap().is_none());
    assert!(handler.handle("abc", &ctx).await.unwrap().is_none());
    assert!(store.list(10).unwrap().is_empty());

    let reply = handler.handle("2510.11111", &ctx).await.unwrap().unwrap();
    assert!(reply.text.contains("精读任务已创建"));
    let tasks = store.list(10).unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].request_key, "arxiv:deep:2510.11111");
    assert_eq!(
        tasks[0].notify.as_ref().unwrap().msg_id.as_deref(),
        Some("m9")
    );
}
