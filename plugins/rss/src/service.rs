use ru41::{
    adapters::sources::feed::{FeedClient, FeedItem},
    domain::NotifyRoute,
    ports::storage::TaskStore,
};
use anyhow::Result;
use serde::Serialize;
use std::sync::Arc;

/// RSS 信息流追踪配置（对应配置文件的 [sources.rss]）。
#[derive(Debug, Clone)]
pub struct RssConfig {
    pub feeds: Vec<String>,
    /// 关键词粗筛（大小写不敏感，匹配标题或摘要）；空列表表示不过滤。
    pub keywords: Vec<String>,
    /// 个人关注方向描述，注入 digest prompt 供 Codex 判断相关性。
    pub interests: String,
    /// 每个源单次最多处理的条目数。
    pub max_items_per_feed: u32,
    pub interval_secs: u64,
    pub backend: String,
    pub notify: Option<NotifyRoute>,
    pub digest_mode: DigestMode,
    /// daily 模式晚报时刻（本地时）。
    pub evening_hour: u32,
    /// daily 模式早报时刻；None 不出早报。
    pub morning_hour: Option<u32>,
    /// 条目最大年龄（天）：发布时间更早的视为旧闻直接丢弃，
    /// 防止聚合类源（CSDN 头条等）的考古帖刷屏日报。默认 2。
    pub max_age_days: u64,
    /// daily 模式入池前是否跑关键词粗筛；false 时新条目全量入池，
    /// 相关性判断完全交给 digest（每天仅 1-2 次 GLM 调用，成本不变）。
    pub pool_keyword_filter: bool,
    /// 标题级丢弃规则（配置里的正则编译产物）：命中即不入池。
    /// 用于源级噪声过滤（V2EX 推广/中转站节点等纯营销）。
    pub drop_title_patterns: Vec<regex::Regex>,
}

impl Default for RssConfig {
    /// 语义默认值与 RssSection::resolved 对齐；测试构造只需写关心的字段。
    fn default() -> Self {
        Self {
            feeds: Vec::new(),
            keywords: Vec::new(),
            interests: String::new(),
            max_items_per_feed: 50,
            interval_secs: 21_600,
            backend: "codex".into(),
            notify: None,
            digest_mode: DigestMode::default(),
            evening_hour: 21,
            morning_hour: None,
            max_age_days: 2,
            pool_keyword_filter: true,
            drop_title_patterns: Vec::new(),
        }
    }
}

/// digest 产出节奏：有候选即精选 vs 攒一天到点出日报。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DigestMode {
    #[default]
    Realtime,
    Daily,
}

impl DigestMode {
    pub fn parse(raw: Option<&str>) -> anyhow::Result<Self> {
        match raw {
            None | Some("realtime") => Ok(Self::Realtime),
            Some("daily") => Ok(Self::Daily),
            Some(other) => {
                anyhow::bail!("unknown digest_mode {other:?} (expected realtime or daily)")
            }
        }
    }
}

/// 一轮轮询的结果报告；单源失败不中断其他源，错误收进 errors。
#[derive(Debug, Serialize)]
pub struct RssPollReport {
    pub feeds: usize,
    pub fetched: usize,
    pub new_items: usize,
    pub candidates: usize,
    pub errors: Vec<String>,
    pub digest_task_id: Option<i64>,
    /// daily 模式候选池当前条数（realtime 恒为 0）。
    pub pool_size: usize,
}

/// RSS 信息流追踪：逐源抓取 → 去重 → 关键词粗筛 → 合成精选 digest 任务。
/// 与论文追踪同一模式，输出口径按设计文档 3.4：发生了什么→为什么重要→与我有关→下一步。
// TODO: 同一事件被多源报道时的聚合（当前由各源独立入 digest，靠 Agent 判断）。
#[derive(Clone)]
pub struct RssTracker {
    store: Arc<dyn TaskStore>,
    client: FeedClient,
    config: RssConfig,
}

impl RssTracker {
    pub fn new(store: Arc<dyn TaskStore>, client: FeedClient, config: RssConfig) -> Self {
        Self {
            store,
            client,
            config,
        }
    }

    pub async fn poll_once(&self) -> Result<RssPollReport> {
        let mut fetched = 0;
        let mut errors = Vec::new();
        let mut items = Vec::new();
        for url in &self.config.feeds {
            // 伪 feed 前缀查注册表（wqai+/devpost+/lablab+/datafountain+ 等），
            // 未命中按 RSS/Atom 处理。接新源只需在 sources 注册表登记。
            let fetched_items = match ru41::adapters::sources::fetch_pseudo(url) {
                Some(future) => future.await,
                None => self.client.fetch(url).await,
            };
            let feed_items = match fetched_items {
                Ok(feed_items) => feed_items,
                Err(error) => {
                    errors.push(format!("{url}: {error:#}"));
                    continue;
                }
            };
            fetched += feed_items.len();
            let cutoff = ru41::util::now() - (self.config.max_age_days as i64) * 86_400;
            items.extend(
                feed_items
                    .into_iter()
                    // 源级噪声过滤：标题命中丢弃规则的不入池（比 digest 拦截省 LLM 调用）
                    .filter(|item| {
                        !self
                            .config
                            .drop_title_patterns
                            .iter()
                            .any(|re| re.is_match(&item.title))
                    })
                    .take(self.config.max_items_per_feed as usize)
                    .filter(|item| match parse_item_time(&item.published) {
                        Some(ts) => ts >= cutoff,
                        // 日期无法解析的不丢（多数源日期正常，漏解析不等于旧闻），
                        // 交给 digest 判断。
                        None => true,
                    })
                    .map(|item| (url.clone(), item)),
            );
        }
        let keywords = self.config.keywords.clone();
        let interests = self.config.interests.clone();
        if self.config.digest_mode == DigestMode::Daily {
            let (outcome, pool_size) = self.poll_daily(items, &keywords, &interests)?;
            return Ok(RssPollReport {
                feeds: self.config.feeds.len(),
                fetched,
                new_items: outcome.new_items,
                candidates: outcome.candidates,
                errors,
                digest_task_id: outcome.digest_task_id,
                pool_size,
            });
        }
        let outcome = ru41::app::digest::register_digest(
            ru41::app::digest::DigestSink {
                store: &self.store,
                seen_kind: "rss_seen",
                backend: &self.config.backend,
                notify: self.config.notify.as_ref(),
            },
            items,
            ru41::app::digest::DigestHooks {
                // 键带源前缀，避免不同源的 guid 互相碰撞。
                id: &|(url, item): &(String, FeedItem)| format!("{url}#{}", item.id),
                seen_value: &|(_, item): &(String, FeedItem)| {
                    serde_json::json!({
                        "title": item.title,
                        "link": item.link,
                        "first_seen": ru41::util::now(),
                    })
                },
                keep: &|(_, item): &(String, FeedItem)| {
                    ru41::app::digest::keyword_match(
                        &keywords,
                        &format!("{} {}", item.title, item.summary),
                    )
                },
                key: &|candidates: &[(String, FeedItem)]| {
                    format!(
                        "rss:digest:{}:{:08x}",
                        ru41::util::today_utc(ru41::util::now()),
                        digest_hash(candidates)
                    )
                },
                prompt: &|candidates: &[(String, FeedItem)]| {
                    digest_prompt(
                        &interests,
                        &candidates
                            .iter()
                            .map(|(_, item)| item.clone())
                            .collect::<Vec<_>>(),
                    )
                },
            },
        )?;
        Ok(RssPollReport {
            feeds: self.config.feeds.len(),
            fetched,
            new_items: outcome.new_items,
            candidates: outcome.candidates,
            errors,
            digest_task_id: outcome.digest_task_id,
            pool_size: 0,
        })
    }

    /// daily 模式：去重 + 关键词筛后进候选池（records 按天分组），
    /// 到早/晚报时刻才合成一次 digest 任务。GLM 调用从每批一次降到每天 1-2 次。
    fn poll_daily(
        &self,
        items: Vec<(String, FeedItem)>,
        keywords: &[String],
        interests: &str,
    ) -> Result<(ru41::app::digest::DigestOutcome, usize)> {
        // 1. 去重（与 register_digest 同一套 rss_seen 语义）
        let mut fresh = Vec::new();
        for (url, item) in items {
            let seen_key = format!("{url}#{}", item.id);
            if self.store.document("rss_seen", &seen_key)?.is_some() {
                continue;
            }
            self.store.put_document(
                "rss_seen",
                &seen_key,
                None,
                &serde_json::json!({
                    "title": item.title,
                    "link": item.link,
                    "first_seen": ru41::util::now(),
                }),
            )?;
            fresh.push(item);
        }
        let new_items = fresh.len();
        // pool_keyword_filter=false 时全量入池：关键词粗筛容易误杀标题不含
        // 关键词但实质相关的条目（尤其英文源摘要稀薄），交给 digest 判断更准。
        let candidates: Vec<FeedItem> = if self.config.pool_keyword_filter {
            fresh
                .into_iter()
                .filter(|item| {
                    ru41::app::digest::keyword_match(
                        keywords,
                        &format!("{} {}", item.title, item.summary),
                    )
                })
                .collect()
        } else {
            fresh
        };
        let candidate_count = candidates.len();

        // 2. 入池：当天的池文档追加（按条目 id 去重，封顶 80 条保最新）
        let (date, hour) = local_date_hour(ru41::util::now());
        let pool_id = format!("pool:{date}");
        let pool_doc = self.store.document("rss_pool", &pool_id)?;
        let mut pool: Vec<FeedItem> = pool_doc
            .as_ref()
            .and_then(|doc| serde_json::from_value(doc.value["items"].clone()).ok())
            .unwrap_or_default();
        let mut pool_ids: std::collections::HashSet<String> =
            pool.iter().map(|i| i.id.clone()).collect();
        for item in candidates {
            if pool_ids.insert(item.id.clone()) {
                pool.push(item);
            }
        }
        if pool.len() > 80 {
            pool.drain(..pool.len() - 80);
        }
        self.save_pool(&pool_id, &pool)?;

        // 3. 到点出报：早报/晚报各自一天一份（request_key 天然幂等），出报即清池
        let mut digest_task_id = None;
        for (slot, slot_hour) in [
            ("morning", self.config.morning_hour),
            ("evening", Some(self.config.evening_hour)),
        ] {
            let Some(slot_hour) = slot_hour else { continue };
            // 早报只在晚报时刻之前出（hour >= 8 是一整天，不卡上界会导致
            // 晚上 21 点先出一份"早报"再出晚报）；晚报只卡下界。
            if hour < slot_hour || pool.is_empty() {
                continue;
            }
            if slot == "morning" && hour >= self.config.evening_hour {
                continue;
            }
            let key = format!("rss:digest:{slot}:{date}");
            // 今天的这份报可能已出过（同 key）：prompt 后续迭代会让幂等检查
            // 报「key 冲突」，所以先按 key 查存在性——存在即视为已出报，跳过。
            let existing = self
                .store
                .list(500)?
                .into_iter()
                .find(|task| task.request_key == key);
            if let Some(task) = existing {
                digest_task_id = Some(task.id);
                continue;
            }
            let mut service = ru41::app::task::TaskService::new(self.store.clone());
            let prompt = digest_prompt(interests, &pool);
            let result = match &self.config.notify {
                Some(route) => {
                    service.enqueue_with_notify(&key, &prompt, &self.config.backend, route)?
                }
                None => service.enqueue_for(&key, &prompt, &self.config.backend)?,
            };
            if result.created {
                digest_task_id = Some(result.task.id);
                // 出报即清池：晚报只覆盖早报之后的新候选，早报覆盖隔夜积累。
                self.save_pool(&pool_id, &[])?;
                pool.clear();
            } else {
                // 今天这份已建过（重复轮询/重启），不重复建任务。
                digest_task_id = Some(result.task.id);
            }
        }

        Ok((
            ru41::app::digest::DigestOutcome {
                new_items,
                candidates: candidate_count,
                digest_task_id,
            },
            pool.len(),
        ))
    }

    /// 池文档落盘：put_document(None) 是 INSERT ON CONFLICT DO NOTHING（仅新建），
    /// 已存在的池必须带 revision 走 CAS 更新，否则写入被静默丢弃——池子会
    /// 冻在当天第一次写入的状态（历史 bug）。CAS 冲突（并发轮询）重读重试一次。
    fn save_pool(&self, pool_id: &str, items: &[FeedItem]) -> Result<()> {
        let value = || serde_json::json!({ "items": items });
        let doc = self.store.document("rss_pool", pool_id)?;
        let expected = doc.as_ref().map(|d| d.revision);
        if self
            .store
            .put_document("rss_pool", pool_id, expected, &value())?
        {
            return Ok(());
        }
        let doc = self.store.document("rss_pool", pool_id)?;
        let expected = doc.as_ref().map(|d| d.revision);
        self.store
            .put_document("rss_pool", pool_id, expected, &value())?;
        Ok(())
    }
}

/// 条目发布时间宽容解析：RFC 2822（RSS 2.0）/ RFC 3339（Atom）/ 纯日期。
/// 聚合类源的日期格式不统一，全部试一遍；都失败返回 None 由调用方决定。
fn parse_item_time(published: &str) -> Option<i64> {
    let raw = published.trim();
    if raw.is_empty() {
        return None;
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc2822(raw) {
        return Some(dt.timestamp());
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(raw) {
        return Some(dt.timestamp());
    }
    // "2026-10-24" / "2026-10-24 08:00:00" 类裸格式，按 UTC+8 解释。
    for fmt in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M:%S", "%Y-%m-%d"] {
        if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(raw, fmt) {
            return Some(naive.and_utc().timestamp() - 8 * 3600);
        }
        if let Ok(date) = chrono::NaiveDate::parse_from_str(raw, fmt) {
            return date
                .and_hms_opt(0, 0, 0)
                .map(|dt| dt.and_utc().timestamp() - 8 * 3600);
        }
    }
    None
}

/// 北京时间（UTC+8）的日期与小时：日报时刻按用户本地时区判断。
fn local_date_hour(now: i64) -> (String, u32) {
    use chrono::{TimeZone, Timelike};
    let tz = chrono::FixedOffset::east_opt(8 * 3600).expect("valid offset");
    let dt = tz.timestamp_opt(now, 0).single().expect("valid timestamp");
    (dt.format("%Y-%m-%d").to_string(), dt.hour())
}

/// 候选集合的稳定散列，用于 digest 幂等键（同一批重复 poll 不重复建任务）。
fn digest_hash(candidates: &[(String, FeedItem)]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for (url, item) in candidates {
        url.hash(&mut hasher);
        item.id.hash(&mut hasher);
    }
    hasher.finish()
}

/// 资讯精选 prompt：输出口径按设计文档 3.4 的四段式，区分事实与推断。
fn digest_prompt(interests: &str, candidates: &[FeedItem]) -> String {
    // 任务入队校验上限是 65536 字节（app/task validate_input），池子随源增多
    // 会涨破它——这里给 prompt 留 6 万字节预算：单条摘要截断、超预算截尾。
    const PROMPT_BUDGET: usize = 60_000;
    const SUMMARY_MAX: usize = 400;
    let mut prompt = format!(
        "你是信息筛选助手，正在为用户出一份日报。以下是最新采集到的资讯条目（含标题与摘要级内容）。\n\
         个人关注方向：{interests}\n\n\
         时效纪律：这是日报，优先近 48 小时的条目；发布时间超过 3 天的视为旧闻，\n\
         除非仍在行动窗口内（如未过截止日期/仍可领取），否则一律舍弃并简要说明。\n\
         请筛选出真正有信息增量、与关注方向相关的条目（搬运、纯营销、无实质内容的直接舍弃），\n\
         对每条输出：标题 / 链接 / 发生了什么 / 为什么重要 / 与我有关之处 / 建议的下一步行动。\n\
         区分事实与推断，事实必须能在所给材料中找到依据。\n\
         数字纪律（最高优先级）：金额、人数、日期、百分比等所有数字必须逐字取自材料；\n\
         材料写「未标注」就如实写「未标注」，材料没有的数字一律写「材料未给出」，\n\
         绝对禁止凭常识、经验或事件名称脑补任何数字（宁可不写，不可编造）。\n\
         舍弃纪律：不要逐条罗列舍弃理由或展开分析被舍弃的条目；\n\
         个别接近但不够格的条目至多一句话带过。\n\
         如果全部不值得关注，只回复一行：📭 今日无值得关注的机会。不要附加任何解释。\n\n\
         == 候选条目（{} 条）==\n",
        candidates.len()
    );
    for (i, item) in candidates.iter().enumerate() {
        let entry = format!(
            "\n---\n{}\n链接：{}\n发布：{}\n内容：{}\n",
            item.title,
            item.link,
            item.published,
            ru41::util::truncate_chars(&item.summary, SUMMARY_MAX)
        );
        if prompt.len() + entry.len() > PROMPT_BUDGET {
            prompt.push_str(&format!(
                "\n（其余 {} 条超出长度预算，本次略去）\n",
                candidates.len() - i
            ));
            break;
        }
        prompt.push_str(&entry);
    }
    prompt
}

impl ru41::ingress::schedule::PollJob for RssTracker {
    fn job_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.config.interval_secs)
    }

    fn poll_json(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = anyhow::Result<serde_json::Value>> + Send + '_>,
    > {
        let owned = self.clone();
        ru41::plugins::bridge(async move { Ok(serde_json::to_value(owned.poll_once().await?)?) })
    }
}

#[cfg(test)]
mod tests {
    use super::{DigestMode, FeedItem, RssConfig, RssTracker, local_date_hour, parse_item_time};

    #[test]
    fn parses_common_publish_formats() {
        // RFC 2822（RSS 2.0 标准）
        assert!(parse_item_time("Sat, 24 Oct 2026 08:00:00 GMT").is_some());
        // RFC 3339（Atom 标准）
        assert!(parse_item_time("2026-10-24T08:00:00Z").is_some());
        // 裸日期/裸时间（部分中文聚合源）
        assert!(parse_item_time("2026-10-24").is_some());
        assert!(parse_item_time("2026-10-24 08:00:00").is_some());
        // 垃圾与空串不解析
        assert!(parse_item_time("").is_none());
        assert!(parse_item_time("昨天").is_none());
    }
    use ru41::adapters::sources::feed::FeedClient;
    use ru41::ports::storage::TaskStore;
    use std::sync::Arc;

    fn item(id: &str) -> (String, FeedItem) {
        (
            "https://feed".into(),
            FeedItem {
                id: id.into(),
                title: format!("AI 机会 {id}"),
                link: format!("https://x/{id}"),
                summary: "免费额度".into(),
                published: "2026-10-24".into(),
            },
        )
    }

    fn daily_tracker(store: Arc<dyn TaskStore>, evening_hour: u32) -> RssTracker {
        RssTracker::new(
            store,
            FeedClient::new(),
            RssConfig {
                feeds: vec!["https://feed".into()],
                keywords: vec!["AI".into()],
                interests: "测试关注方向".into(),
                digest_mode: DigestMode::Daily,
                evening_hour,
                ..Default::default()
            },
        )
    }

    #[test]
    fn daily_pool_fires_digest_at_hour_and_dedups() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(
            ru41::adapters::database::sqlite::SqliteTaskStore::open(&dir.path().join("s.db"))
                .unwrap(),
        );
        // evening_hour = 0 → 任何时刻都到点：入池即出报并清池。
        let tracker = daily_tracker(store.clone(), 0);
        let (outcome, pool) = tracker
            .poll_daily(vec![item("a"), item("b")], &["AI".to_owned()], "i")
            .unwrap();
        assert_eq!(outcome.new_items, 2);
        assert_eq!(outcome.candidates, 2);
        assert!(outcome.digest_task_id.is_some(), "到点应出报");
        assert_eq!(pool, 0, "出报后清池");
        let tasks = store.list(10).unwrap();
        assert_eq!(tasks.len(), 1);
        let (date, _) = local_date_hour(ru41::util::now());
        assert_eq!(tasks[0].request_key, format!("rss:digest:evening:{date}"));

        // 重复轮询：条目已 seen，候选 0，不重复建任务。
        let (outcome, _) = tracker
            .poll_daily(vec![item("a"), item("b")], &["AI".to_owned()], "i")
            .unwrap();
        assert_eq!(outcome.new_items, 0);
        assert_eq!(store.list(10).unwrap().len(), 1);
    }

    #[test]
    fn daily_before_evening_hour_only_pools() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(
            ru41::adapters::database::sqlite::SqliteTaskStore::open(&dir.path().join("s.db"))
                .unwrap(),
        );
        let (_, hour) = local_date_hour(ru41::util::now());
        if hour == 23 {
            return; // 23 点后无更晚时刻可测，跳过
        }
        let tracker = daily_tracker(store.clone(), hour + 1);
        let (outcome, pool) = tracker
            .poll_daily(vec![item("a")], &["AI".to_owned()], "i")
            .unwrap();
        assert_eq!(outcome.candidates, 1);
        assert!(outcome.digest_task_id.is_none(), "未到点不出报");
        assert_eq!(pool, 1, "候选留在池中");
        assert_eq!(store.list(10).unwrap().len(), 0);
    }

    #[test]
    fn digest_prompt_enforces_quiet_empty_report() {
        let prompt = super::digest_prompt("关注免费额度", &[item("a").1]);
        assert!(prompt.contains("📭 今日无值得关注的机会"));
        assert!(prompt.contains("不要附加任何解释"));
        assert!(prompt.contains("不要逐条罗列舍弃理由"));
    }

    #[test]
    fn digest_prompt_stays_within_enqueue_budget() {
        // 大量长摘要条目也不能顶破任务入队的 65536 字节上限
        let mut big = Vec::new();
        for i in 0..500 {
            let (url, mut it) = item(&format!("n{i}"));
            it.summary = "长".repeat(2000);
            big.push((url, it));
        }
        let candidates: Vec<super::FeedItem> = big.into_iter().map(|(_, it)| it).collect();
        let prompt = super::digest_prompt("i", &candidates);
        assert!(prompt.len() <= 65_536, "实际 {} 字节", prompt.len());
        assert!(prompt.contains("超出长度预算"));
    }

    #[test]
    fn daily_pool_keyword_filter_off_pools_everything() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(
            ru41::adapters::database::sqlite::SqliteTaskStore::open(&dir.path().join("s.db"))
                .unwrap(),
        );
        let mut tracker = daily_tracker(store.clone(), 0);
        tracker.config.pool_keyword_filter = false;
        // 标题不含关键词 "AI" 的条目也应入池。
        let mut unrelated = item("z");
        unrelated.1.title = "社区闲聊帖".into();
        let (outcome, pool) = tracker
            .poll_daily(vec![unrelated], &["AI".to_owned()], "i")
            .unwrap();
        assert_eq!(outcome.candidates, 1, "关粗筛后全量入池");
        assert_eq!(pool, 0, "evening_hour=0 到点出报清池");
        assert_eq!(store.list(10).unwrap().len(), 1);
    }

    #[test]
    fn daily_pool_accumulates_across_polls() {
        // 回归：put_document(None) 是仅新建（ON CONFLICT DO NOTHING），池文档
        // 已存在时写入会被静默丢弃，池子冻在当天首轮状态。修复后第二轮
        // poll 的条目必须真正落进库存的池文档。
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(
            ru41::adapters::database::sqlite::SqliteTaskStore::open(&dir.path().join("s.db"))
                .unwrap(),
        );
        let (_, hour) = local_date_hour(ru41::util::now());
        if hour == 23 {
            return;
        }
        let tracker = daily_tracker(store.clone(), hour + 1);
        tracker
            .poll_daily(vec![item("a")], &["AI".to_owned()], "i")
            .unwrap();
        tracker
            .poll_daily(vec![item("b")], &["AI".to_owned()], "i")
            .unwrap();
        let (date, _) = local_date_hour(ru41::util::now());
        let doc = store
            .document("rss_pool", &format!("pool:{date}"))
            .unwrap()
            .expect("池文档存在");
        let items: Vec<FeedItem> = serde_json::from_value(doc.value["items"].clone()).unwrap();
        let ids: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["a", "b"], "两轮 poll 的条目都在池里");
    }
}
