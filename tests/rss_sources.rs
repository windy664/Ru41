use ru41::{
    adapters::database::sqlite::SqliteTaskStore,
    adapters::sources::feed::{FeedClient, parse_feed, parse_rss},
    domain::NotifyRoute,
    modules::rss::{RssConfig, RssTracker},
    ports::storage::TaskStore,
};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    sync::Arc,
};

/// pubDate 用当前时间生成：轮询路径有时效闸（默认丢弃 2 天前的条目），
/// 固定旧日期的夹具会被正确拦掉，导致测试随时间腐烂。
fn sample_rss() -> String {
    let now = chrono::DateTime::from_timestamp(ru41::util::now(), 0).unwrap();
    let pub_date = now.format("%a, %d %b %Y %H:%M:%S GMT");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>示例安全资讯</title>
    <item>
      <title>某大模型 Agent 框架曝工具调用漏洞</title>
      <link>https://example.com/news/1</link>
      <guid>https://example.com/news/1</guid>
      <pubDate>{pub_date}</pubDate>
      <description><![CDATA[<p>研究人员披露了 <b>tool-use</b> 安全问题 &amp; 修复建议</p>]]></description>
    </item>
    <item>
      <title>某手机品牌发布新配色</title>
      <link>https://example.com/news/2</link>
      <pubDate>{pub_date}</pubDate>
      <description>营销内容</description>
    </item>
  </channel>
</rss>"#
    )
}

#[test]
fn parses_rss_feed_items() {
    let items = parse_rss(&sample_rss()).unwrap();
    assert_eq!(items.len(), 2);
    let first = &items[0];
    assert_eq!(first.id, "https://example.com/news/1");
    assert_eq!(first.title, "某大模型 Agent 框架曝工具调用漏洞");
    // CDATA 里的 HTML 已剥离，实体已还原。
    assert_eq!(first.summary, "研究人员披露了 tool-use 安全问题 & 修复建议");
    // 第二条没有 guid，退回 link 作 id。
    assert_eq!(items[1].id, "https://example.com/news/2");
}

#[test]
fn rejects_unknown_feed_formats() {
    assert!(parse_rss("<html><body>not a feed</body></html>").is_err());
}

fn sample_atom() -> String {
    let now = chrono::DateTime::from_timestamp(ru41::util::now(), 0).unwrap();
    let stamp = now.format("%Y-%m-%dT%H:%M:%SZ");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>示例 Atom 源</title>
  <entry>
    <id>urn:example:post-1</id>
    <title>LLM Agent 安全研究综述发布</title>
    <link rel="alternate" href="https://example.com/posts/1"/>
    <link rel="enclosure" href="https://example.com/posts/1.pdf"/>
    <published>{stamp}</published>
    <updated>{stamp}</updated>
    <summary>覆盖 prompt injection 与 tool-use 安全。</summary>
  </entry>
  <entry>
    <id>urn:example:post-2</id>
    <title>某品牌手机评测</title>
    <link href="https://example.com/posts/2"/>
    <updated>{stamp}</updated>
    <content type="html">&lt;p&gt;营销内容&lt;/p&gt;</content>
  </entry>
</feed>"#
    )
}

#[test]
fn parses_atom_feed_entries() {
    let items = parse_feed(&sample_atom()).unwrap();
    assert_eq!(items.len(), 2);
    let first = &items[0];
    assert_eq!(first.id, "urn:example:post-1");
    assert_eq!(first.title, "LLM Agent 安全研究综述发布");
    // rel=alternate 才是正文链接，enclosure 不覆盖。
    assert_eq!(first.link, "https://example.com/posts/1");
    assert!(first.published.ends_with('Z'));
    assert!(first.summary.contains("prompt injection"));
    // content 兜底摘要，HTML 实体已还原并去标签。
    let second = &items[1];
    assert_eq!(second.link, "https://example.com/posts/2");
    assert_eq!(second.summary, "营销内容");
}

#[tokio::test]
async fn poll_works_with_atom_feed() {
    let base = mock_rss_server(&sample_atom());
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let tracker = make_tracker(&store, vec![base], vec!["安全".into()]);

    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.fetched, 2);
    assert_eq!(report.candidates, 1);
    let task = store.get(report.digest_task_id.unwrap()).unwrap().unwrap();
    assert!(task.prompt.contains("LLM Agent 安全研究综述发布"));
    assert!(!task.prompt.contains("手机评测"));
}

/// 本地 mock：任何 GET 都返回样本 feed。
fn mock_rss_server(payload: &str) -> String {
    let payload = payload.to_owned();
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
                "HTTP/1.1 200 OK\r\nContent-Type: application/rss+xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                payload.len(),
                payload
            )
            .unwrap();
        }
    });
    base
}

fn make_tracker(
    store: &Arc<dyn TaskStore>,
    feeds: Vec<String>,
    keywords: Vec<String>,
) -> RssTracker {
    RssTracker::new(
        store.clone(),
        FeedClient::new(),
        RssConfig {
            feeds,
            keywords,
            interests: "AI 安全方向本科生".into(),
            max_items_per_feed: 50,
            interval_secs: 21_600,
            backend: "codex".into(),
            notify: Some(NotifyRoute {
                channel: "feishu".into(),
                target: "default".into(),
                msg_id: None,
            }),
            digest_mode: ru41::modules::rss::service::DigestMode::Realtime,
            evening_hour: 21,
            morning_hour: None,
            max_age_days: 2,
            pool_keyword_filter: true,
        },
    )
}

#[tokio::test]
async fn poll_enqueues_digest_and_dedups() {
    let base = mock_rss_server(&sample_rss());
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let tracker = make_tracker(&store, vec![base.clone()], vec![]);

    let first = tracker.poll_once().await.unwrap();
    assert_eq!(first.fetched, 2);
    assert_eq!(first.candidates, 2);
    assert!(first.errors.is_empty());
    let task = store.get(first.digest_task_id.unwrap()).unwrap().unwrap();
    assert!(task.request_key.starts_with("rss:digest:"));
    assert!(task.prompt.contains("某大模型 Agent 框架曝工具调用漏洞"));
    assert!(task.prompt.contains("区分事实与推断"));
    assert_eq!(task.notify.unwrap().channel, "feishu");

    // 第二轮：全部已见，不再入队。
    let second = tracker.poll_once().await.unwrap();
    assert_eq!(second.new_items, 0);
    assert_eq!(second.digest_task_id, None);
    assert_eq!(store.list(10).unwrap().len(), 1);
}

#[tokio::test]
async fn keyword_filter_and_feed_failure_isolation() {
    let good = mock_rss_server(&sample_rss());
    let bad = {
        // 绑定后立即丢弃监听，连接会被拒绝。
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        addr
    };
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let tracker = make_tracker(&store, vec![bad, good], vec!["tool-use".into()]);

    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.feeds, 2);
    assert_eq!(report.errors.len(), 1);
    // 关键词只命中第一条，营销条目被过滤。
    assert_eq!(report.candidates, 1);
    let task = store.get(report.digest_task_id.unwrap()).unwrap().unwrap();
    assert!(task.prompt.contains("tool-use"));
    assert!(!task.prompt.contains("新配色"));
}
