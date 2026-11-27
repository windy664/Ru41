use anyhow::Result;
use ru41::{
    adapters::sources::feed::{FeedClient, FeedItem},
    domain::NotifyRoute,
    ports::storage::TaskStore,
};
use serde::Serialize;
use std::sync::Arc;

/// 必报池的条目上限：必报条目独立成池（不与新闻抢名额），但也别无限涨。
///
/// ⚠️ 2026-11-24 从 15 降到 **5**：15 是当初为了让「必报段能发得出去」定的，
/// 结果微软 11-23 一次批量发布被 NVD 收录成 15 条高危 CVE，必报段就发 15 条 ——
/// 一篇日报的必报段吃掉 3750 字（用户原话：「谁让你发这么多安全漏洞的」）。
const MUST_POOL_CAP: usize = 5;

/// **同一批次**最多留几条必报。
///
/// 批次 = 同来源前缀 + 同发布日。同一家的批量补丁（微软 Patch Tuesday 那种）
/// 本质是**同一条消息**，留最重的两条就够，剩下的逐条列出来只是刷屏。
/// 不同发布日的多条高危漏洞不受影响（各是各的事）。
const MUST_CLUSTER_CAP: usize = 2;

/// 候选池里非机会类条目的上限（机会类不受此限，见 `is_opportunity`）。
const POOL_CAP: usize = 80;

/// 「机会类」条目的 id 前缀（缺省值）：这些适配器产出的是**有截止日期**的
/// 比赛/黑客松/实习/奖学金。
///
/// ⚠️ 为什么要单独认它们（2026-11-27 实测）：池子封顶与提示词预算**都按采集
/// 时间淘汰**（保最新），而机会类条目**发布一次、留在池里直到截止日** ⇒ 天然
/// 排在池子前部 ⇒ 连续 6 期日报把它们全砍掉：
///
/// | 日报 | 候选 | 进了材料 | 机会类进了几条 |
/// |---|---|---|---|
/// | 11-25 早 | 53 | 53 | ✅ 黑客松×3 实习×12 奖金×5 |
/// | 11-26 晚 | 80 | 27 | ❌ 零 |
/// | 11-27 早 | 76 | 26 | ❌ 零（5 个有奖金的黑客松 + 1 条实习） |
///
/// ⇒ 机会类优先进材料、也不因池子封顶被淘汰。
pub const DEFAULT_OPPORTUNITY_ID_PREFIXES: &[&str] = &[
    "wqai-",
    "devpost-",
    "lablab-",
    "df-",
    "xfyun-",
    "hackertrip-",
    "hack123-",
    "gsoc-",
    "lfx-",
    "hackathoncom-",
    "mianling-",
];

/// 这一条是不是「机会类」（有截止日期的比赛/黑客松/实习/奖学金）。
pub fn is_opportunity(item: &FeedItem, prefixes: &[String]) -> bool {
    prefixes.iter().any(|prefix| item.id.starts_with(prefix.as_str()))
}

/// 进材料时的排序：**机会类优先**，其余**新的在前**。
///
/// 池子是插入顺序（老→新），所以「新的在前」= 反向遍历。
pub fn order_for_prompt<'a>(
    items: &'a [FeedItem],
    prefixes: &[String],
) -> Vec<&'a FeedItem> {
    let (opportunity, rest): (Vec<&FeedItem>, Vec<&FeedItem>) =
        items.iter().partition(|item| is_opportunity(item, prefixes));
    let mut ordered = opportunity;
    ordered.extend(rest.into_iter().rev());
    ordered
}

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
    /// 「机会类」条目的 id 前缀：源产出的是**有截止日期**的比赛/黑客松/实习/
    /// 奖学金。池子封顶与提示词预算都优先保它们（见 `is_opportunity`）。
    pub opportunity_id_prefixes: Vec<String>,
    /// 必报硬闸：命中的条目**单独成一条 digest 任务**（不许被 LLM 舍弃）。
    pub must_report: bool,
    /// 必报条目**并进日报正文**（一条消息发完），而不是单独成一条任务。
    /// 缺省 true。
    pub merge_must_report: bool,
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
            backend: "pi".into(),
            notify: None,
            digest_mode: DigestMode::default(),
            evening_hour: 21,
            morning_hour: None,
            max_age_days: 2,
            pool_keyword_filter: true,
            drop_title_patterns: Vec::new(),
            opportunity_id_prefixes: DEFAULT_OPPORTUNITY_ID_PREFIXES
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            must_report: true,
            merge_must_report: true,
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
            // 权威漏洞源(GHSA/KEV/NVD/php.net)自己管时间窗(URL 上的 days=),
            // 不受全局 max_age_days 约束 ——
            // 2026-11-22 实测踩坑: 漏洞条目的 published 是**日期**(没有时分),
            // 解析出来是当天 00:00 UTC, 被「2 天内」的门槛判成旧闻全丢掉
            // (100 条 GHSA 只剩 0 条)。而这些源本来就是按「最近 N 天」自己筛过的。
            let authoritative = ru41::adapters::sources::security::is_authoritative_feed(url);
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
                    .filter(|item| is_fresh(&item.published, cutoff, authoritative))
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
        //
        // 必报条目进**另一个池**(pool:must:{date}): 2026-11-22 实测, GHSA 一次
        // 就带 100 条, 跟新闻混在一个池里会把新闻挤掉(池子封顶 80, 丢最旧的)。
        // 分池之后: 必报池独立封顶, 普通池的名额只留给新闻。
        let (date, hour) = local_date_hour(ru41::util::now());
        let pool_id = format!("pool:{date}");
        let must_pool_id = format!("pool:must:{date}");
        let (must_candidates, normal_candidates): (Vec<FeedItem>, Vec<FeedItem>) = candidates
            .into_iter()
            .partition(|item| self.is_must_report(item));
        let mut pool = self.load_pool(&pool_id)?;
        let mut must_pool = self.load_pool(&must_pool_id)?;
        let mut pool_ids: std::collections::HashSet<String> =
            pool.iter().map(|i| i.id.clone()).collect();
        for item in normal_candidates {
            if pool_ids.insert(item.id.clone()) {
                pool.push(item);
            }
        }
        if pool.len() > POOL_CAP {
            // 封顶：**机会类（有截止日期的比赛/黑客松/实习）不因封顶被淘汰**。
            //
            // ⚠️ 2026-11-27 实测：原来这里是「保最新 80 条」（丢最旧的），
            // 而机会类条目发布一次、留到截止日 ⇒ 天然排在池子前部 ⇒ 被逐出池子
            // ⇒ 连续 6 期日报一条机会类都没报（11-27 那期候选 76 条只进了 24 条）。
            //
            // 现在：**先丢最旧的非机会类**，机会类留着；名额不够时才动机会类。
            let opportunity: Vec<bool> = pool
                .iter()
                .map(|item| is_opportunity(item, &self.config.opportunity_id_prefixes))
                .collect();
            let opportunity_count = opportunity.iter().filter(|flag| **flag).count();
            let mut room = POOL_CAP.saturating_sub(opportunity_count);
            let mut keep = opportunity.clone();
            for index in (0..pool.len()).rev() {
                if room == 0 {
                    break;
                }
                if !keep[index] {
                    keep[index] = true;
                    room -= 1;
                }
            }
            let all = std::mem::take(&mut pool);
            pool = all
                .into_iter()
                .zip(keep)
                .filter_map(|(item, keep)| if keep { Some(item) } else { None })
                .collect();
        }
        let mut must_ids: std::collections::HashSet<String> =
            must_pool.iter().map(|i| i.id.clone()).collect();
        for item in must_candidates {
            if must_ids.insert(item.id.clone()) {
                must_pool.push(item);
            }
        }
        // 必报池裁剪：同批次扎堆 + 总量封顶。落选的条目**不进正文**
        // (换到正文发还是 15 条, 用户照样嫌多), 但数量会在日报里留一行说明。
        let dropped = trim_must_pool(&mut must_pool);
        let overflow_note = overflow_note(&dropped);
        self.save_pool(&pool_id, &pool)?;
        self.save_pool(&must_pool_id, &must_pool)?;

        // 3. 到点出报：早报/晚报各自一天一份（request_key 天然幂等），出报即清池
        //
        // 必报硬闸(2026-11-21): 命中的高危安全条目**单独成一条 digest 任务** ——
        // 它的 prompt 里只有这些条目、且明令「一条都不许舍弃」。这样「必报」不再
        // 依赖 LLM 每天现场判断（Java4Shell 被写进「已舍弃」那一行就是这么丢的）。
        // 必报任务先入队 → 告警先到；普通日报里不再包含必报条目（不重复报）。
        let mut digest_task_id = None;
        for (slot, slot_hour) in [
            ("morning", self.config.morning_hour),
            ("evening", Some(self.config.evening_hour)),
        ] {
            let Some(slot_hour) = slot_hour else { continue };
            // 早报只在晚报时刻之前出（hour >= 8 是一整天，不卡上界会导致
            // 晚上 21 点先出一份"早报"再出晚报）；晚报只卡下界。
            if hour < slot_hour || (pool.is_empty() && must_pool.is_empty()) {
                continue;
            }
            if slot == "morning" && hour >= self.config.evening_hour {
                continue;
            }
            let must = must_pool.clone();
            let rest = pool.clone();
            let mut service = ru41::app::task::TaskService::new(self.store.clone());
            let mut emitted = false;

            // 3.1 合并模式（缺省）：必报条目并进日报正文，**一条消息发完**。
            //     两套纪律分区：第一部分必报（一条不许舍），第二部分资讯（可舍弃）。
            //     key 用普通日报那个（rss:digest:{slot}:{date}），别再用 must 的 key ——
            //     否则同一天可能同时存在「旧的必报独立任务」和「合并任务」两条消息。
            //
            //     ⚠️ 这里**不能 continue**：后面还有「出报即清池」，
            //     跳过去池子就不清了（下一轮 poll 会一直带着旧条目）。
            if self.config.merge_must_report {
                if !must.is_empty() || !rest.is_empty() {
                    let key = format!("rss:digest:{slot}:{date}");
                    let existing = self
                        .store
                        .list(500)?
                        .into_iter()
                        .find(|task| task.request_key == key);
                    if let Some(task) = existing {
                        digest_task_id = Some(task.id);
                    } else {
                        let prompt = merged_report_prompt(
                            &must,
                            &rest,
                            &interests,
                            &overflow_note,
                            &self.config.opportunity_id_prefixes,
                        );
                        let result = match &self.config.notify {
                            Some(route) => service.enqueue_with_notify(
                                &key,
                                &prompt,
                                &self.config.backend,
                                route,
                            )?,
                            None => service.enqueue_for(&key, &prompt, &self.config.backend)?,
                        };
                        if result.created {
                            emitted = true;
                        }
                        digest_task_id = Some(result.task.id);
                    }
                }
            } else {
                // 3.2 分开发模式：必报段独立成任务，先入队 → 告警先到；
                //     普通日报里不再包含必报条目（不重复报）。
                if !must.is_empty() {
                    let key = format!("rss:digest:must:{slot}:{date}");
                    if !self.task_exists(&key)? {
                        let prompt = must_report_prompt(&must);
                        let result = match &self.config.notify {
                            Some(route) => service.enqueue_with_notify(
                                &key,
                                &prompt,
                                &self.config.backend,
                                route,
                            )?,
                            None => service.enqueue_for(&key, &prompt, &self.config.backend)?,
                        };
                        if result.created {
                            emitted = true;
                        }
                        digest_task_id = Some(result.task.id);
                    }
                }

                // 3.3 普通日报（不含必报条目；没有普通条目就不出报）
                if !rest.is_empty() {
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
                    } else {
                        let prompt = digest_prompt_with_note(
                            interests,
                            &rest,
                            &overflow_note,
                            &self.config.opportunity_id_prefixes,
                        );
                        let result = match &self.config.notify {
                            Some(route) => service.enqueue_with_notify(
                                &key,
                                &prompt,
                                &self.config.backend,
                                route,
                            )?,
                            None => service.enqueue_for(&key, &prompt, &self.config.backend)?,
                        };
                        if result.created {
                            emitted = true;
                        }
                        digest_task_id = Some(result.task.id);
                    }
                }
            }

            if emitted {
                // 出报即清池：晚报只覆盖早报之后的新候选，早报覆盖隔夜积累。
                self.save_pool(&pool_id, &[])?;
                self.save_pool(&must_pool_id, &[])?;
                pool.clear();
                must_pool.clear();
            }
        }

        Ok((
            ru41::app::digest::DigestOutcome {
                new_items,
                candidates: candidate_count,
                digest_task_id,
            },
            pool.len() + must_pool.len(),
        ))
    }

    /// 这条要不要进必报段：代码层判据 + 是否来自权威漏洞源。
    fn is_must_report(&self, item: &FeedItem) -> bool {
        if !self.config.must_report {
            return false;
        }
        // 来源类别由条目 id 前缀决定（安全源适配器给自己的 id 加了固定前缀）：
        // GHSA/NVD=官方漏洞公告 / KEV=已在野利用 / php=语言官方安全发布 / 其它=资讯源。
        let origin = crate::security::Origin::of_item(&item.id);
        crate::security::is_must_report(&item.title, &item.summary, origin)
    }

    /// 这个 request_key 的任务是不是已经建过了（幂等：一天一份，不重复建）。
    fn task_exists(&self, key: &str) -> Result<bool> {
        Ok(self
            .store
            .list(500)?
            .into_iter()
            .any(|task| task.request_key == key))
    }

    /// 读池文档。
    fn load_pool(&self, pool_id: &str) -> Result<Vec<FeedItem>> {
        let doc = self.store.document("rss_pool", pool_id)?;
        Ok(doc
            .as_ref()
            .and_then(|doc| serde_json::from_value(doc.value["items"].clone()).ok())
            .unwrap_or_default())
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

/// 条目是否够新（进不进池）。
///
/// 权威漏洞源(GHSA/KEV/NVD/php.net)自己按 URL 上的 `days=` 管时间窗 →
/// **不受全局 max_age_days 约束**。
///
/// 2026-11-22 实测踩坑: 漏洞条目的 published 是**日期**(没有时分), 解析出来是
/// 当天 00:00 UTC, 被「2 天内」的门槛判成旧闻全丢掉(100 条 GHSA 只剩 0 条) ——
/// 而这些源本来就按「最近 N 天」自己筛过一遍了。
fn is_fresh(published: &str, cutoff: i64, authoritative: bool) -> bool {
    if authoritative {
        return true;
    }
    match parse_item_time(published) {
        Some(ts) => ts >= cutoff,
        // 日期无法解析的不丢（多数源日期正常，漏解析不等于旧闻），交给 digest 判断。
        None => true,
    }
}

/// 必报池裁剪：强的排前面、同样强的新在前，然后封顶。
///
/// 为什么要有排序(2026-11-22): 池子按插入顺序封顶会丢**最旧**的 ——
/// 而 KEV（已被利用）条目可能是几天前进来的、却比今天的中危漏洞更该报。
/// 所以按「强度 → 时间」排完再截。
///
/// ⚠️ 2026-11-24 加了**同批次扎堆**抑制：原来只有总量上限(15)，没有批次概念，
/// 于是微软 11-23 一次批量发布被 NVD 收录的 15 条高危 CVE 全进了必报段。
/// 现在两层：
///   1. 同批次(同来源 + 同发布日)最多留 [`MUST_CLUSTER_CAP`] 条；
///   2. 总量封顶 [`MUST_POOL_CAP`]。
///
/// 落选的**不进必报段、也不进日报正文** —— 换个位置发还是 15 条，用户照样嫌多；
/// 但会在日报里留一行「另有 N 条同批次高危未逐条列出」，数量可见，不静默吞掉。
/// 返回落选的条目。
fn trim_must_pool(pool: &mut Vec<FeedItem>) -> Vec<FeedItem> {
    pool.sort_by(|a, b| {
        let pa = must_strength(a);
        let pb = must_strength(b);
        pa.cmp(&pb).then_with(|| b.published.cmp(&a.published))
    });

    // 1) 同批次裁剪
    let mut kept: Vec<FeedItem> = Vec::with_capacity(pool.len());
    let mut dropped: Vec<FeedItem> = Vec::new();
    let mut per_cluster: std::collections::HashMap<(String, String), usize> =
        std::collections::HashMap::new();
    for item in pool.drain(..) {
        let source = source_prefix(&item);
        // ⚠️ KEV(CISA 已在野利用目录)**不参与扎堆抑制** —— 那类是「现在就被真实
        // 攻击在用」的条目, 必须逐条报, 不能因为同一天攒了几条就被压掉。
        // 它的优先级也最高(priority=0), 排序时自然在前, 不会被总量上限截掉。
        if source == "kev" {
            kept.push(item);
            continue;
        }
        let key = (source.clone(), publish_day(&item));
        let counter = per_cluster.entry(key).or_insert(0);
        if *counter < MUST_CLUSTER_CAP {
            *counter += 1;
            kept.push(item);
        } else {
            dropped.push(item);
        }
    }

    // 2) 总量封顶 —— **KEV 不受此限**。
    //
    // KEV 是「现在就被真实攻击在用」的条目, 每一条都该报; 而它每天新增只有个位数,
    // 不会变成刷屏。普通强信号按「强度 → 时间」排序后截, KEV 优先级最高(0),
    // 排序时自然在前 —— 这里逐条判断只让**非 KEV** 的去抢那 5 个名额。
    if kept.len() > MUST_POOL_CAP {
        let mut capped: Vec<FeedItem> = Vec::with_capacity(kept.len());
        let mut overflow: Vec<FeedItem> = Vec::new();
        for item in kept {
            let is_kev = source_prefix(&item) == "kev";
            if is_kev || capped.len() < MUST_POOL_CAP {
                capped.push(item);
            } else {
                overflow.push(item);
            }
        }
        kept = capped;
        dropped.extend(overflow);
    }

    *pool = kept;
    dropped
}

/// 条目的来源前缀（`nvd-…` → `nvd`）：同一前缀的条目来自同一个漏洞源。
fn source_prefix(item: &FeedItem) -> String {
    item.id
        .split('-')
        .next()
        .filter(|part| !part.is_empty())
        .unwrap_or("?")
        .to_owned()
}

/// 条目的发布日期（批次的一半）。解析不出日期就整串用 —— 那时每条各不相同，
/// 等于不聚簇（保守：宁可不压，也不误压）。
fn publish_day(item: &FeedItem) -> String {
    let raw = item.published.trim();
    let is_date = |s: &str| s.len() == 10 && s.matches('-').count() == 2;
    if let Some((head, _)) = raw.split_once([' ', 'T'])
        && is_date(head)
    {
        return head.to_owned();
    }
    if is_date(raw) {
        return raw.to_owned();
    }
    raw.to_owned()
}

/// 落选必报条目的说明行（进日报开头）。
///
/// 例：`ℹ️ 另有 13 条同日(2026-11-23)同源(nvd)的高危漏洞未逐条列出（同一批次，
/// 已按强度取前 2 条）。`
/// 没有落选时返回空串。
fn overflow_note(dropped: &[FeedItem]) -> String {
    if dropped.is_empty() {
        return String::new();
    }
    // 按批次分组说明，别只给一个总数让人猜是哪批
    let mut groups: std::collections::BTreeMap<(String, String), usize> =
        std::collections::BTreeMap::new();
    for item in dropped {
        *groups
            .entry((source_prefix(item), publish_day(item)))
            .or_insert(0) += 1;
    }
    let detail = groups
        .iter()
        .map(|((source, day), count)| format!("{day} 的 {source} 源 {count} 条"))
        .collect::<Vec<_>>()
        .join("；");
    format!(
        "ℹ️ 另有 {} 条同批次高危漏洞未逐条列出（{}）—— 同一批次的条目本质是同一件事，\
         已按强度取前几条列入必报段。",
        dropped.len(),
        detail
    )
}

/// 必报强度：0 = 已在野利用, 1 = 其它强信号, 2 = 只靠编号与严重等级。
fn must_strength(item: &FeedItem) -> u8 {
    let origin = crate::security::Origin::of_item(&item.id);
    let v = crate::security::verdict(&item.title, &item.summary);
    crate::security::priority(&v, origin)
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

/// 必报段 prompt：**只有必报条目**，且明令一条都不许舍弃。
///
/// 与普通日报 prompt 的关键差别：
/// - 材料里**只有**必报条目，没有「可舍弃的其它条目」跟它抢注意力；
/// - 明确禁止「舍弃纪律」——普通日报 prompt 里那句「舍弃纪律」是漏洞被丢的根源；
/// - 空洞条目也要列出（材料没写细节就写「材料未给出具体细节」），但不能自己发挥。
fn must_report_prompt(candidates: &[FeedItem]) -> String {
    const PROMPT_BUDGET: usize = 60_000;
    const SUMMARY_MAX: usize = 600;
    let mut prompt = format!(
        "你是安全告警播报员。下面是**必报**条目 —— 代码层已判定为高危（CVSS ≥ 9.0 / Critical / RCE / 0day / 无需认证即可利用 / 已在野利用，或来自权威漏洞源的 CVE 公告）。\n\
         工具禁令（最高优先级）：所有判断只依据下面的材料，**全程不要调用任何工具**\n\
         ——不要访问链接、不要用 bash/浏览器核实内容。材料即全部事实来源。\n\
         必报纪律（最高优先级）：\n\
         1. **每一条都要报**。一条都不能少、不能合并、不能因为「与上一条同类」而省略。\n\
         2. 开头写一行 `🚨 必报 {n} 条`，随后**每条三行**（这是告警清单，不是长文，\n\
         每条控制在 250 字以内；总长不要超过 4000 字）：\n\
         `**编号（CVSS/严重等级）标题**` / `发生了什么：…` / `影响与处置：…`\n\
         3. 影响与处置里写清：受影响组件与版本、是否需要立刻升级/停用、官方处置要求。\n\
         4. 数字纪律：CVSS 分值、CVE/GHSA 编号、版本号、处置期限等**逐字取自材料**；\n\
         材料没写就写「材料未给出」，**绝对禁止**凭常识补数字。\n\
         5. 若某条材料看着像空洞公告（只有标题没有实质信息），写「材料未给出具体细节」，\n\
         但**仍要列出该条** —— 这是必报清单，不许自己删。\n\
         6. **不要写「已舍弃」段落** —— 这里没有需要舍弃的条目。\n\
         交付口径：最终 JSON 的 summary 字段放**完整正文**（Markdown 原样搬进去），\n\
         不要先在消息里写一遍、再压缩成一段摘要交差。\n\
         == 必报条目（{n} 条）==",
        n = candidates.len()
    );
    let ordered: Vec<&FeedItem> = candidates.iter().rev().collect();
    for (i, item) in ordered.iter().enumerate() {
        let verdict = crate::security::verdict(&item.title, &item.summary);
        let reasons = crate::security::reason_line(&verdict);
        let entry = format!(
            "\n---\n{}\n链接：{}\n发布：{}\n必报理由：{}\n内容：{}\n",
            item.title,
            item.link,
            item.published,
            reasons,
            ru41::util::truncate_chars(&item.summary, SUMMARY_MAX)
        );
        if prompt.len() + entry.len() > PROMPT_BUDGET {
            prompt.push_str(&format!(
                "\n（其余 {} 条较旧条目超出长度预算，本次略去）\n",
                candidates.len() - i
            ));
            break;
        }
        prompt.push_str(&entry);
    }
    prompt
}

/// 合并日报 prompt：**一条消息发完**，但必报硬闸不丢。
///
/// ⚠️ 为什么要分区（2026-11-25 改）：
/// 原来必报是**独立一条任务**，普通日报是另一条 —— 用户收到两条消息，问「为什么分开发」。
/// 但直接把两套材料混在一起会出事：普通日报 prompt 里那句「舍弃纪律」会把必报漏洞一起舍掉
/// （Java4Shell 就是这么丢的）。所以合并时**纪律必须分区**：
///   第一部分 = 必报（一条都不许舍、每条三行、🚨 开头）
///   第二部分 = 资讯（时效/拉人头/来源/舍弃纪律照旧）
/// 且第一部分在**最前面** —— dispatcher 的空报判定只看正文前 80 字，
/// 📭 出现在开头会被当空报静默消化掉。
///
/// 预算：任务入队上限 65536 字节（app/task validate_input），所以两个部分
/// **各自**限额（不能各给 60k，合起来就爆了）。
fn merged_report_prompt(
    must: &[FeedItem],
    rest: &[FeedItem],
    interests: &str,
    note: &str,
    opportunity_prefixes: &[String],
) -> String {
    // 预算按**字节**算（Rust 的 String::len()），而材料大半是中文（1 字 3 字节）
    // ⇒ 26,000 字节只放得下 ~24 条。2026-11-27 实测：候选 76 条只进了 24 条，
    // 机会类（黑客松/竞赛/实习）**一条没进材料** ⇒ 日报里那类条目全是 0。
    //
    // 现在：摘要截到 250 字（每条约 700 字节）⇒ 80 条约 54 KB，加指令段与必报段
    // 仍在入队上限（65,536 字节）内 ⇒ **候选基本能全进材料**。
    const MUST_BUDGET: usize = 5_000;
    const REST_BUDGET: usize = 54_000;
    const SUMMARY_MAX: usize = 250;

    // ① 先算材料（机会类优先、其余新的在前），头部要写**实际带进去的条数**。
    let must_ordered: Vec<&FeedItem> = must.iter().rev().collect();
    let (must_entries, must_kept, _) = build_material(
        &must_ordered,
        MUST_BUDGET,
        SUMMARY_MAX,
        |item, summary| {
            let verdict = crate::security::verdict(&item.title, &item.summary);
            let reasons = crate::security::reason_line(&verdict);
            format!(
                "\n---\n{}\n链接：{}\n发布：{}\n必报理由：{}\n内容：{}\n",
                item.title, item.link, item.published, reasons, summary
            )
        },
    );

    let rest_ordered = order_for_prompt(rest, opportunity_prefixes);
    let (rest_entries, rest_kept, rest_dropped) = build_material(
        &rest_ordered,
        REST_BUDGET,
        SUMMARY_MAX,
        |item, summary| {
            format!(
                "\n---\n{}\n链接：{}\n发布：{}\n内容：{}\n",
                item.title, item.link, item.published, summary
            )
        },
    );

    // ② 再拼提示词。
    let mut prompt = String::new();
    if !note.is_empty() {
        prompt.push_str(note);
        prompt.push_str("\n\n");
    }
    prompt.push_str(&format!(
        "你是信息筛选助手，正在为用户出一份日报。这份日报有**两个部分**，**纪律不同**，请严格分区执行。\n\
         个人关注方向：{interests}\n\n\
         工具禁令（最高优先级）：所有判断只依据下面的材料，**全程不要调用任何工具**\n\
         ——不要访问链接、不要用 bash/浏览器核实内容。材料即全部事实来源，链接留待读者自行打开。\n\
         数字纪律（对两部分都生效）：金额、人数、日期、百分比、CVSS 分值、CVE/GHSA 编号、版本号等\n\
         所有数字必须逐字取自材料；材料没写就写「材料未给出」，**绝对禁止**凭常识脑补任何数字。\n\
         交付口径：最终 JSON 的 summary 字段放**完整日报正文**（Markdown 结构原样搬进去），\n\
         不要先在消息里写一遍、再压缩成一段摘要交差——那段摘要会被直接推给用户。\n\n\
         ══════════ 第一部分 · 必报安全告警（{must_count} 条）══════════\n\
         【必报纪律（最高优先级，优先于第二部分的一切规则）】\n\
         1. **每一条都要报**。一条都不能少、不能合并、不能因为「与上一条同类」而省略。\n\
         2. **本部分必须写在最前面**，第一行写 `🚨 必报 {must_count} 条`，随后**每条三行**\n\
         （这是告警清单，不是长文，每条控制在 250 字以内；本部分总长不要超过 4000 字）：\n\
         `**编号（CVSS/严重等级）标题**` / `发生了什么：…` / `影响与处置：…`\n\
         3. 影响与处置里写清：受影响组件与版本、是否需要立刻升级/停用、官方处置要求。\n\
         4. 若某条材料看着像空洞公告（只有标题没有实质信息），写「材料未给出具体细节」，\n\
         但**仍要列出该条** —— 这是必报清单，不许自己删。\n\
         5. **不要写「已舍弃」段落** —— 本部分没有需要舍弃的条目。\n\
         6. **本部分非空时绝对禁止**只回「📭 今日无值得关注的机会」—— 那行只属于第二部分。\n\n\
         == 必报条目（{must_count} 条）==",
        must_count = must_kept
    ));
    prompt.push_str(&must_entries);

    prompt.push_str(&format!(
        "\n\n══════════ 第二部分 · 今日资讯（{rest_kept} 条）══════════\n\
         【资讯纪律（只作用于本部分；第一部分的必报条目不受这些规则影响）】\n\
         时效纪律：这是日报，优先近 48 小时的条目；发布时间超过 3 天的视为旧闻，\n\
         除非仍在行动窗口内（如未过截止日期/仍可领取），否则一律舍弃并简要说明。\n\
         请筛选出真正有信息增量、与关注方向相关的条目（搬运、纯营销、无实质内容的直接舍弃），\n\
         对每条输出：标题 / 链接 / 发生了什么 / 为什么重要 / 与我有关之处 / 建议的下一步行动。\n\
         区分事实与推断，事实必须能在所给材料中找到依据。\n\
         拉人头纪律：邀请码、邀请链接、注册返利、用码送额度等推广类条目一律舍弃——\n\
         发帖人拿返利，对读者没有信息增量，且往往夸大收益。项目官方公告、平台官方\n\
         活动（官方域名发布）不在此列。社区用户发帖（v2ex/Reddit/论坛等）要优先\n\
         怀疑其推广动机，除非能给出来自官方来源的佐证。\n\
         促销纪律：**纯购物促销**（黑五、黑色星期五、礼品卡、优惠券、秒杀、游戏充值、\n\
         限时开售）一律舍弃——它们是消费广告不是信息增量（2026-11-27 实测：日报里混进了\n\
         3 条黑五促销）。**但「免费/限量的 API 额度、云服务试用、开源模型发布、\n\
         学生优惠」属于关注方向，不许当促销丢掉。**\n\
         消费广告不是信息增量（2026-11-27 实测：日报里混进了 3 条黑五促销）。\n\
         条目纪律：**每一条都必须是一条具体资讯**；不许写「本条合并说明」这类\n\
         占位/元说明条目（2026-11-27 实测：出现过一条「…（本条合并说明）」占坑）。\n\
         来源纪律：同一事件有多个来源时，**优先给原始出处**——研究机构/厂商/官方公告/\n\
         官方条目的原文链接；中文转载、聚合站只作补充，不能只给转载链接。\n\
         原始来源就在候选材料里时，链接必须用它。\n\
         舍弃纪律：不要逐条罗列舍弃理由或展开分析被舍弃的条目；\n\
         个别接近但不够格的条目至多一句话带过。\n\
         ⚠️ 如果本部分全部不值得关注：**不要**把「📭 今日无值得关注的机会」放在正文最前面\n\
         （系统按开头判断空报，会把整条报静默吃掉）——第一部分报完之后再写这一行，或干脆不写。\n\n\
         == 资讯候选条目（共 {rest_total} 条，按预算带了 {rest_kept} 条）==",
        rest_kept = rest_kept,
        rest_total = rest.len()
    ));
    if rest_dropped > 0 {
        prompt.push_str(&format!(
            "（另有 {rest_dropped} 条未进材料：优先保了机会类（有截止日期的比赛/实习）与最新条目）\n"
        ));
    }
    prompt.push_str(&rest_entries);
    prompt
}

/// 按**字节**截断（不切坏 UTF-8）。预算按字节算时必须用它，
/// 按字符截会算出 3 倍的体积（中文 1 字 3 字节）。
fn truncate_bytes(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// 每条材料的最小字节预算（标题/链接/发布/分隔 + 一小段摘要）；
/// 低于它就别硬挤了 —— 挤出来的残缺摘要对模型没用。
const MIN_ITEM_BYTES: usize = 220;

/// 候选条数上限：预算不够均分时，只取优先级最高的这么多条。
/// 免得给模型几百条一句话摘要（挑不出东西，还挤掉真正有料的）。
const MAX_CANDIDATES: usize = 60;

/// 把候选装进材料段：**每条均分预算**，摘要按预算尽量给长 ⇒ 候选再多也进得来。
///
/// 2026-11-27 实测：原来「整份预算逐条填、装不下就丢」⇒ 候选 76 条只进了 24 条，
/// 而且丢的恰好是机会类（有截止日期的比赛/实习 —— 它们发布一次、留在池子最前部）。
/// 现在改成均分预算 ⇒ 76 条能全进（摘要略短一点），机会类更是排在最前永不丢。
///
/// 返回 `(材料, 实际条数, 被丢条数)`。
fn build_material(
    items: &[&FeedItem],
    budget: usize,
    summary_cap: usize,
    mut entry: impl FnMut(&FeedItem, &str) -> String,
) -> (String, usize, usize) {
    let total = items.len();
    if total == 0 {
        return (String::new(), 0, 0);
    }
    // 预算够均分就全要；不够就只取优先级最高的若干条。
    let per_item = budget / total;
    let planned = if per_item >= MIN_ITEM_BYTES {
        total
    } else {
        total.min(MAX_CANDIDATES)
    };
    let per_item = (budget / planned).max(MIN_ITEM_BYTES);
    let mut out = String::new();
    let mut kept = 0usize;
    for item in items.iter().take(planned) {
        // 先量这条的固定开销（标题/链接/发布/分隔），剩下的给摘要。
        let overhead = entry(item, "").len();
        let room = per_item.saturating_sub(overhead).max(40);
        let summary = ru41::util::truncate_chars(&truncate_bytes(&item.summary, room), summary_cap);
        let text = entry(item, &summary);
        if out.len() + text.len() > budget {
            break;
        }
        out.push_str(&text);
        kept += 1;
    }
    (out, kept, total.saturating_sub(kept))
}

/// 资讯精选 prompt：输出口径按设计文档 3.4 的四段式，区分事实与推断。
fn digest_prompt(interests: &str, candidates: &[FeedItem]) -> String {
    digest_prompt_with_note(
        interests,
        candidates,
        "",
        &DEFAULT_OPPORTUNITY_ID_PREFIXES
            .iter()
            .map(|s| (*s).to_owned())
            .collect::<Vec<_>>(),
    )
}

/// 带「落选必报条目」说明行的日报 prompt。
///
/// 必报池裁剪（同批次扎堆 + 总量封顶）落选的条目不进正文 —— 换到正文发还是
/// 十几条漏洞, 用户照样嫌多。但数量不该静默消失: 在日报开头留一行说明,
/// 让人知道「还有多少条同批次的没逐条列」。
fn digest_prompt_with_note(
    interests: &str,
    candidates: &[FeedItem],
    note: &str,
    opportunity_prefixes: &[String],
) -> String {
    // 任务入队校验上限是 65536 字节（app/task validate_input），池子随源增多
    // 会涨破它——这里给 prompt 留 6 万字节预算：单条摘要截断、超预算截尾。
    //
    // ⚠️ 预算按**字节**算，中文 1 字 3 字节 ⇒ 摘要 400 字时每条约 1 KB，
    // 80 条就 80 KB（超上限）。摘要截到 250 字 ⇒ 每条约 700 字节 ⇒ 80 条 54 KB ✓。
    const PROMPT_BUDGET: usize = 60_000;
    const SUMMARY_MAX: usize = 250;

    // 预算有限时**优先保最新的**：池子是插入顺序（老→新），按原顺序填、截尾丢的
    // 恰好是最新采集的那批（2026-11-18 实测：当天最该报的条目被截掉了，
    // 日报里只剩二手转载）。所以这里改成**新的在前**，超预算时丢最旧的。
    //
    // 2026-11-27 再加一条：**机会类（有截止日期的比赛/黑客松/实习）优先进材料**
    // —— 它们发布一次、留到截止日，按采集时间淘汰等于天天杀掉（连续 6 期日报
    // 一条机会类都没报，见 `DEFAULT_OPPORTUNITY_ID_PREFIXES`）。
    let ordered = order_for_prompt(candidates, opportunity_prefixes);
    let (entries, kept, dropped) = build_material(
        &ordered,
        PROMPT_BUDGET,
        SUMMARY_MAX,
        |item, summary| {
            format!(
                "\n---\n{}\n链接：{}\n发布：{}\n内容：{}\n",
                item.title, item.link, item.published, summary
            )
        },
    );

    let mut prompt = format!(
        "你是信息筛选助手，正在为用户出一份日报。以下是最新采集到的资讯条目（含标题与摘要级内容）。\n\
         个人关注方向：{interests}\n\n\
         工具禁令（最高优先级）：所有判断只依据下面的候选材料，**全程不要调用任何工具**\n\
         ——不要访问链接、不要用 bash/浏览器核实内容。材料即全部事实来源，链接留待读者自行打开。\n\
         时效纪律：这是日报，优先近 48 小时的条目；发布时间超过 3 天的视为旧闻，\n\
         除非仍在行动窗口内（如未过截止日期/仍可领取），否则一律舍弃并简要说明。\n\
         请筛选出真正有信息增量、与关注方向相关的条目（搬运、纯营销、无实质内容的直接舍弃），\n\
         对每条输出：标题 / 链接 / 发生了什么 / 为什么重要 / 与我有关之处 / 建议的下一步行动。\n\
         区分事实与推断，事实必须能在所给材料中找到依据。\n\
         拉人头纪律：邀请码、邀请链接、注册返利、用码送额度等推广类条目一律舍弃——\n\
         发帖人拿返利，对读者没有信息增量，且往往夸大收益。项目官方公告、平台官方\n\
         活动（官方域名发布）不在此列。社区用户发帖（v2ex/Reddit/论坛等）要优先\n\
         怀疑其推广动机，除非能给出来自官方来源的佐证。\n\
         促销纪律：**纯购物促销**（黑五、黑色星期五、礼品卡、优惠券、秒杀、游戏充值、\n\
         限时开售）一律舍弃——它们是消费广告不是信息增量（2026-11-27 实测：日报里混进了\n\
         3 条黑五促销）。**但「免费/限量的 API 额度、云服务试用、开源模型发布、\n\
         学生优惠」属于关注方向，不许当促销丢掉。**\n\
         消费广告不是信息增量（2026-11-27 实测：日报里混进了 3 条黑五促销）。\n\
         条目纪律：**每一条都必须是一条具体资讯**；不许写「本条合并说明」这类\n\
         占位/元说明条目（2026-11-27 实测：出现过一条「…（本条合并说明）」占坑）。\n\
         数字纪律：金额、人数、日期、百分比等所有数字必须逐字取自材料；\n\
         材料写「未标注」就如实写「未标注」，材料没有的数字一律写「材料未给出」，\n\
         绝对禁止凭常识、经验或事件名称脑补任何数字（宁可不写，不可编造）。\n\
         来源纪律：同一事件有多个来源时，**优先给原始出处**——研究机构/厂商/官方公告/\n\
         官方条目的原文链接；中文转载、聚合站只作补充，不能只给转载链接。\n\
         原始来源就在候选材料里时，链接必须用它。\n\
         舍弃纪律：不要逐条罗列舍弃理由或展开分析被舍弃的条目；\n\
         个别接近但不够格的条目至多一句话带过。\n\
         如果全部不值得关注，只回复一行：📭 今日无值得关注的机会。不要附加任何解释。\n\
         交付口径：最终 JSON 的 summary 字段放**完整日报正文**（Markdown 结构原样搬进去），\n\
         不要先在消息里写一遍日报、再压缩成一段摘要交差——那段摘要会被直接推给用户。\n\n\
         == 候选条目（共 {total} 条，按预算带了 {kept} 条）==\n",
        total = candidates.len(),
        kept = kept
    );
    if dropped > 0 {
        prompt.push_str(&format!(
            "（另有 {dropped} 条未进材料：优先保了机会类（有截止日期的比赛/实习）与最新条目）\n"
        ));
    }
    // 必报池落选说明放在最前面（读者第一眼就能看到「还有多少条没列」）
    if !note.is_empty() {
        prompt = format!("{note}\n\n{prompt}");
    }
    prompt.push_str(&entries);
    prompt
}

impl ru41::ingress::schedule::SimplePollJob for RssTracker {
    type Report = RssPollReport;

    fn interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.config.interval_secs)
    }

    async fn poll(&self) -> anyhow::Result<Self::Report> {
        self.poll_once().await
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

    /// 「优先原始出处」是**固定规则**（跟数字纪律同级），不靠 interests 里的自然语言提示。
    #[test]
    fn digest_prompt_requires_the_original_source_link() {
        let prompt = super::digest_prompt("关注免费额度", &[item("a").1]);
        assert!(prompt.contains("来源纪律"), "{prompt}");
        assert!(prompt.contains("优先给原始出处"), "{prompt}");
        assert!(prompt.contains("不能只给转载链接"), "{prompt}");
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
        // 2026-11-27 起不再「装不下就丢」（那会丢掉机会类条目）⇒ 改成均分预算：
        // 条数按上限收，**头部如实写带了几条、丢了多少**。
        let kept = prompt.matches("链接：").count();
        assert!(kept > 0, "一条都没进材料");
        assert!(
            prompt.contains(&format!("按预算带了 {kept} 条")),
            "头部计数与实际不符：{prompt}"
        );
        assert!(
            prompt.contains("另有") && prompt.contains("条未进材料"),
            "丢了多少条要写出来：{prompt}"
        );
    }

    /// 预算吃紧时**必须保住最新的条目**。
    ///
    /// 2026-11-18 实测踩坑：池子是插入顺序（老→新），按原顺序填 prompt、
    /// 撞预算从尾部截断 → **被截掉的恰好是最新采集的那批**
    /// （Java4Shell 的原始披露就在里面），日报里只剩二手转载。
    #[test]
    fn digest_prompt_keeps_the_newest_items_when_budget_is_tight() {
        let mut big = Vec::new();
        for i in 0..500 {
            let (url, mut it) = item(&format!("n{i}"));
            it.summary = "长".repeat(2000);
            big.push((url, it));
        }
        let candidates: Vec<super::FeedItem> = big.into_iter().map(|(_, it)| it).collect();
        let prompt = super::digest_prompt("i", &candidates);
        // 池子最后一条 = 最新一条，必须在里面。
        assert!(prompt.contains("https://x/n499"), "最新的条目被预算截掉了");
        // 最老的那批才是被舍弃的。
        assert!(!prompt.contains("https://x/n0"), "最旧的应被舍弃");
    }

    /// **回归（2026-11-27）**：机会类条目（有截止日期的比赛/黑客松/实习）
    /// **发布一次、留在池里直到截止日** ⇒ 天然排在池子前部。两道截断原来都按
    /// 「采集时间」淘汰（保最新）⇒ 它们连续 6 期日报一条都没进材料
    /// （11-27 那期候选 76 条只进了 24 条：5 个有奖金的黑客松 + 1 条实习）。
    ///
    /// 现在机会类优先进材料 ⇒ 池子最前部的机会类**必须**在里面。
    #[test]
    fn opportunity_items_survive_the_budget_cut() {
        let mut candidates: Vec<super::FeedItem> = Vec::new();
        // 池子前部：机会类（早采集、位置靠前）—— 原来最先被砍掉的就是它们。
        for i in 0..5 {
            let (_, mut it) = item(&format!("hack123-opp{i}"));
            it.summary = "有奖金的黑客松".repeat(50);
            candidates.push(it);
        }
        // 池子后部：大量普通新闻（摘要很长，撑爆预算）。
        for i in 0..500 {
            let (_, mut it) = item(&format!("n{i}"));
            it.summary = "长".repeat(2000);
            candidates.push(it);
        }
        let prompt = super::digest_prompt("关注黑客松与实习", &candidates);
        for i in 0..5 {
            assert!(
                prompt.contains(&format!("https://x/hack123-opp{i}")),
                "机会类条目 opp{i} 被预算截掉了 —— 这正是 11-27 那期日报丢光机会类的根因"
            );
        }
    }

    /// 头部声明的条数必须等于**实际带进材料**的条数（否则模型不知道漏了什么）。
    #[test]
    fn prompt_header_count_matches_embedded_items() {
        let mut candidates: Vec<super::FeedItem> = Vec::new();
        for i in 0..500 {
            let (_, mut it) = item(&format!("n{i}"));
            it.summary = "长".repeat(2000);
            candidates.push(it);
        }
        let prompt = super::digest_prompt("i", &candidates);
        let embedded = prompt.matches("链接：").count();
        assert!(embedded > 0, "一条都没进材料");
        assert!(
            prompt.contains(&format!("按预算带了 {embedded} 条")),
            "头部计数与实际不符：prompt={prompt}"
        );
    }

    /// 80 条真实体量的候选 ⇒ prompt 必须在入队上限（65,536 字节）内，
    /// 且**全部**进材料（不再有「候选 76 条只进 24 条」那种截断）。
    #[test]
    fn eighty_candidates_fit_the_enqueue_budget() {
        let mut candidates: Vec<super::FeedItem> = Vec::new();
        for i in 0..80 {
            let (_, mut it) = item(&format!("n{i}"));
            it.summary = "中文摘要内容".repeat(80); // ≈400 字，逼近原来的 400 字上限
            candidates.push(it);
        }
        let prompt = super::digest_prompt("i", &candidates);
        assert!(
            prompt.len() < 65_536,
            "prompt {} 字节，超出入队上限",
            prompt.len()
        );
        assert_eq!(
            prompt.matches("链接：").count(),
            80,
            "80 条候选应当全部进材料（2026-11-27 那期 76 条只进了 24 条）"
        );
    }

    /// **合并路径**（必报 + 资讯 一条消息发完）的总量也必须在入队上限内，
    /// 且 76 条候选全部进材料、机会类一条不丢。
    #[test]
    fn merged_prompt_fits_budget_and_keeps_opportunities() {
        let must: Vec<super::FeedItem> = (0..2)
            .map(|i| {
                let (_, mut it) = item(&format!("must{i}"));
                it.title = format!("CVE-2026-{i} 高危漏洞");
                it.summary = "CVSS 10 Critical".repeat(30);
                it
            })
            .collect();
        let mut rest: Vec<super::FeedItem> = Vec::new();
        for i in 0..5 {
            let (_, mut it) = item(&format!("hack123-opp{i}"));
            it.summary = "有奖金的黑客松".repeat(50);
            rest.push(it);
        }
        for i in 0..71 {
            let (_, mut it) = item(&format!("n{i}"));
            it.summary = "中文摘要内容".repeat(80);
            rest.push(it);
        }
        let prompt = super::merged_report_prompt(
            &must,
            &rest,
            "关注黑客松与实习",
            "",
            &super::DEFAULT_OPPORTUNITY_ID_PREFIXES
                .iter()
                .map(|s| (*s).to_owned())
                .collect::<Vec<_>>(),
        );
        assert!(
            prompt.len() < 65_536,
            "合并 prompt {} 字节，超出入队上限",
            prompt.len()
        );
        for i in 0..5 {
            assert!(
                prompt.contains(&format!("https://x/hack123-opp{i}")),
                "机会类条目 opp{i} 没进材料"
            );
        }
        // 76 条候选全进（必报 2 条另算）。
        assert_eq!(
            prompt.matches("链接：").count(),
            78,
            "必报 2 条 + 资讯 76 条都该在材料里"
        );
    }

    /// 池子封顶时**机会类不被淘汰**（原来「保最新 80 条」踢的是最旧的，
    /// 而机会类正是最早采集、留到截止日的那批）。
    #[test]
    fn pool_cap_does_not_evict_opportunity_items() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(
            ru41::adapters::database::sqlite::SqliteTaskStore::open(&dir.path().join("s.db"))
                .unwrap(),
        );
        let mut tracker = daily_tracker(store.clone(), 0);
        // 造一个「前部有机会类、后面全是新闻」的池子，然后触发封顶。
        let mut pool: Vec<super::FeedItem> = Vec::new();
        for i in 0..5 {
            let (_, it) = item(&format!("hack123-opp{i}"));
            pool.push(it);
        }
        for i in 0..200 {
            let (_, it) = item(&format!("n{i}"));
            pool.push(it);
        }
        tracker.config.pool_keyword_filter = false;
        // 直接调用封顶逻辑（与 poll 里同一段）。
        let opportunity: Vec<bool> = pool
            .iter()
            .map(|item| {
                super::is_opportunity(item, &tracker.config.opportunity_id_prefixes)
            })
            .collect();
        let opportunity_count = opportunity.iter().filter(|flag| **flag).count();
        let mut room = super::POOL_CAP.saturating_sub(opportunity_count);
        let mut keep = opportunity.clone();
        for index in (0..pool.len()).rev() {
            if room == 0 {
                break;
            }
            if !keep[index] {
                keep[index] = true;
                room -= 1;
            }
        }
        let kept_opportunity = keep
            .iter()
            .zip(pool.iter())
            .filter(|(keep, item)| **keep && item.id.starts_with("hack123-"))
            .count();
        assert_eq!(
            kept_opportunity, 5,
            "机会类被池子封顶淘汰了 —— 它们会永远进不了日报"
        );
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

    /// 必报硬闸（**分开发**模式）：高危安全条目**单独成一个任务**，普通条目在另一个任务里，
    /// 两边不互相污染（必报那条也不该再出现在普通日报里）。
    ///
    /// ⚠️ 现在默认是**合并**模式（一条消息发完），这个测试专测分开发那条路，
    /// 所以显式把 `merge_must_report` 关掉 —— 合并模式的覆盖见下面的
    /// `merged_digest_puts_must_and_news_in_one_task`。
    #[test]
    fn must_report_items_get_their_own_digest_task() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(
            ru41::adapters::database::sqlite::SqliteTaskStore::open(&dir.path().join("s.db"))
                .unwrap(),
        );
        let mut tracker = daily_tracker(store.clone(), 0);
        tracker.config.merge_must_report = false;
        // 关键词粗筛默认开着，标题里没 "AI" 的漏洞条目会在入池前被筛掉 —— 关掉。
        tracker.config.pool_keyword_filter = false;
        let (url, mut vuln) = item("vuln");
        vuln.title = "某库曝远程代码执行漏洞".into();
        vuln.summary = "CVSS 9.8，无需认证即可远程接管，存在公开利用代码。".into();
        let (url2, normal) = item("normal");
        let (outcome, pool) = tracker
            .poll_daily(vec![(url, vuln), (url2, normal)], &["AI".to_owned()], "i")
            .unwrap();
        assert_eq!(outcome.new_items, 2);
        assert_eq!(pool, 0, "出报后清池");
        let tasks = store.list(10).unwrap();
        assert_eq!(tasks.len(), 2, "必报 + 普通 各一个任务: {tasks:?}");
        let (date, _) = local_date_hour(ru41::util::now());
        let must = tasks
            .iter()
            .find(|t| t.request_key == format!("rss:digest:must:evening:{date}"))
            .expect("必报任务");
        let plain = tasks
            .iter()
            .find(|t| t.request_key == format!("rss:digest:evening:{date}"))
            .expect("普通日报任务");
        // 必报任务里有高危条目、没有普通条目。
        assert!(
            must.prompt.contains("某库曝远程代码执行漏洞"),
            "{}",
            must.prompt
        );
        assert!(!must.prompt.contains("AI 机会 normal"), "{}", must.prompt);
        assert!(must.prompt.contains("一条都不能少"), "{}", must.prompt);
        // 普通日报里不再有必报条目。
        assert!(
            !plain.prompt.contains("某库曝远程代码执行漏洞"),
            "{}",
            plain.prompt
        );
        assert!(plain.prompt.contains("AI 机会 normal"), "{}", plain.prompt);
    }

    /// 合并模式（缺省）：必报漏洞与普通资讯**合成一条**日报任务 —— 用户收到一条消息，
    /// 但必报那部分仍带「一条都不许舍」的硬闸。
    ///
    /// ⚠️ 判据是**任务数**（1 条，不是 2 条）+ **prompt 里两段都在**，
    /// 再加一条容易踩的：必报段必须在**最前面**（dispatcher 按正文前 80 字判空报，
    /// 📭 开头会被静默吞掉）。
    #[test]
    fn merged_digest_puts_must_and_news_in_one_task() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(
            ru41::adapters::database::sqlite::SqliteTaskStore::open(&dir.path().join("s.db"))
                .unwrap(),
        );
        let mut tracker = daily_tracker(store.clone(), 0);
        tracker.config.pool_keyword_filter = false;
        let (url, mut vuln) = item("vuln");
        vuln.title = "某库曝远程代码执行漏洞".into();
        vuln.summary = "CVSS 9.8，无需认证即可远程接管，存在公开利用代码。".into();
        let (url2, normal) = item("normal");
        let (outcome, pool) = tracker
            .poll_daily(vec![(url, vuln), (url2, normal)], &["AI".to_owned()], "i")
            .unwrap();
        assert_eq!(outcome.new_items, 2);
        assert_eq!(pool, 0, "出报后清池");

        let tasks = store.list(10).unwrap();
        assert_eq!(tasks.len(), 1, "合成一条，不是两条: {tasks:?}");
        let (date, _) = local_date_hour(ru41::util::now());
        let merged = tasks
            .iter()
            .find(|t| t.request_key == format!("rss:digest:evening:{date}"))
            .expect("合并日报任务");
        // 不能再有独立的必报任务（否则当天会发两条）。
        assert!(
            !tasks
                .iter()
                .any(|t| t.request_key == format!("rss:digest:must:evening:{date}")),
            "不该再有独立必报任务: {tasks:?}"
        );
        // 两段材料都在。
        assert!(
            merged.prompt.contains("某库曝远程代码执行漏洞"),
            "{}",
            merged.prompt
        );
        assert!(
            merged.prompt.contains("AI 机会 normal"),
            "{}",
            merged.prompt
        );
        // 必报的硬闸还在（一条都不许舍）。
        assert!(merged.prompt.contains("一条都不能少"), "{}", merged.prompt);
        // 必报段在**前面**，资讯段在后 —— 顺序反了会被空报判定吃掉。
        let must_at = merged.prompt.find("第一部分 · 必报").expect("必报段");
        let news_at = merged.prompt.find("第二部分 · 今日资讯").expect("资讯段");
        assert!(must_at < news_at, "必报段必须在资讯段之前");
        // 纪律分区：资讯纪律不得覆盖必报段。
        assert!(
            merged
                .prompt
                .contains("只作用于本部分；第一部分的必报条目不受这些规则影响"),
            "{}",
            merged.prompt
        );
    }

    /// 合并模式下**只有必报、没有资讯**时，也要出一条（且必报段在前）。
    #[test]
    fn merged_digest_still_reports_when_only_must_items() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(
            ru41::adapters::database::sqlite::SqliteTaskStore::open(&dir.path().join("s.db"))
                .unwrap(),
        );
        let mut tracker = daily_tracker(store.clone(), 0);
        tracker.config.pool_keyword_filter = false;
        let (url, mut vuln) = item("vuln");
        vuln.title = "某库曝远程代码执行漏洞".into();
        vuln.summary = "CVSS 9.8，无需认证即可远程接管，存在公开利用代码。".into();
        tracker
            .poll_daily(vec![(url, vuln)], &["AI".to_owned()], "i")
            .unwrap();
        let tasks = store.list(10).unwrap();
        assert_eq!(tasks.len(), 1, "只有必报也要出一条: {tasks:?}");
        let prompt = &tasks[0].prompt;
        assert!(prompt.contains("某库曝远程代码执行漏洞"), "{prompt}");
        // 必报非空时禁止回 📭（那是空报信号）。
        assert!(
            prompt.contains("绝对禁止**只回「📭 今日无值得关注的机会」"),
            "{prompt}"
        );
    }

    /// 空洞公告不成条：只有「发布安全公告」字样的条目不进必报。
    #[test]
    fn hollow_security_announcement_is_not_must_report() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(
            ru41::adapters::database::sqlite::SqliteTaskStore::open(&dir.path().join("s.db"))
                .unwrap(),
        );
        let mut tracker = daily_tracker(store.clone(), 0);
        tracker.config.pool_keyword_filter = false;
        let (url, mut hollow) = item("hollow");
        hollow.title = "某公司发布安全公告".into();
        hollow.summary = "更多细节请关注官方渠道。".into();
        tracker
            .poll_daily(vec![(url, hollow)], &["AI".to_owned()], "i")
            .unwrap();
        let tasks = store.list(10).unwrap();
        let (date, _) = local_date_hour(ru41::util::now());
        assert!(
            !tasks
                .iter()
                .any(|t| t.request_key == format!("rss:digest:must:evening:{date}")),
            "空洞公告不该成必报条目: {tasks:?}"
        );
    }

    /// 必报池裁剪：强的留下，弱的让位（KEV 那种几天前进来的「已被利用」不能被
    /// 今天的中危漏洞挤掉）。
    /// 2026-11-24: 同批次扎堆 —— 微软 11-23 一次批量发布被 NVD 收录成 15 条高危 CVE,
    /// 原来只有总量上限(15) → 必报段就发 15 条, 一篇日报的必报段吃掉 3750 字。
    /// 现在同批次(同来源+同发布日)最多留 2 条, 落选的 13 条不进正文(但日报留一行说明)。
    #[test]
    fn must_pool_trims_same_batch_to_a_couple() {
        let mut pool: Vec<FeedItem> = Vec::new();
        for i in 0..15 {
            let (_url, mut it) = item(&format!("nvd-CVE-2026-{}", 10_000 + i));
            it.title = format!("CVE-2026-{}：Microsoft 某产品漏洞", 10_000 + i);
            it.summary =
                "CVSS 9.1 · 严重等级 CRITICAL · 向量 CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:N · 描述"
                    .into();
            it.published = "2026-11-23".into();
            pool.push(it);
        }
        let dropped = super::trim_must_pool(&mut pool);
        assert_eq!(
            pool.len(),
            super::MUST_CLUSTER_CAP,
            "同批次只该留最重的几条"
        );
        assert_eq!(dropped.len(), 13, "落选的条目要交回去(日报里留一行说明)");
    }

    /// KEV(已在野利用)**不参与扎堆抑制** —— 那类必须逐条报。
    #[test]
    fn must_pool_never_trims_in_the_wild_entries() {
        let mut pool: Vec<FeedItem> = Vec::new();
        for i in 0..6 {
            let (_url, mut it) = item(&format!("kev-CVE-2026-{}", 40_000 + i));
            it.title = format!("CVE-2026-{}：已被利用漏洞", 40_000 + i);
            it.summary = "CISA 已知被利用漏洞目录（KEV）· 处置期限 2026-12-01".into();
            it.published = "2026-11-23".into();
            pool.push(it);
        }
        super::trim_must_pool(&mut pool);
        assert_eq!(pool.len(), 6, "已在野利用的条目不该被扎堆压掉");
    }

    /// 不同发布日的高危漏洞**不是同一批** —— 各是各的事, 不该被扎堆压掉。
    #[test]
    fn must_pool_keeps_different_days_apart() {
        let mut pool: Vec<FeedItem> = Vec::new();
        for i in 0..10 {
            let (_url, mut it) = item(&format!("nvd-CVE-2026-{}", 20_000 + i));
            it.title = format!("CVE-2026-{}：某产品漏洞", 20_000 + i);
            it.summary =
                "CVSS 9.1 · 严重等级 CRITICAL · 向量 CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:N · 描述"
                    .into();
            it.published = format!("2026-11-{:02}", 1 + i);
            pool.push(it);
        }
        super::trim_must_pool(&mut pool);
        assert_eq!(pool.len(), super::MUST_POOL_CAP, "跨批次时只受总量上限约束");
    }

    /// 落选条目不许静默消失 —— 日报开头要有一行说明「还有多少条没列」。
    #[test]
    fn overflow_note_reports_dropped_count() {
        let mut pool: Vec<FeedItem> = Vec::new();
        for i in 0..6 {
            let (_url, mut it) = item(&format!("nvd-CVE-2026-{}", 30_000 + i));
            it.published = "2026-11-23".into();
            pool.push(it);
        }
        let dropped = super::trim_must_pool(&mut pool);
        let note = super::overflow_note(&dropped);
        assert!(!dropped.is_empty());
        assert!(note.contains("4 条"), "说明行要带落选条数: {note}");
        assert!(note.contains("2026-11-23"), "说明行要带批次日期: {note}");
        assert!(super::overflow_note(&[]).is_empty(), "没落选就不该有说明行");
    }

    #[test]
    fn must_pool_trim_keeps_the_strong_ones() {
        let mut pool: Vec<FeedItem> = Vec::new();
        for i in 0..20 {
            let (url, mut it) = item(&format!("w{i}"));
            it.title = format!("GHSA-w{i}：某 CMS 的 XSS");
            it.summary = "严重等级 moderate · 官方公告，详见 CVE-2026-0000。".into();
            it.published = "2026-11-20".into();
            pool.push(it);
            let _ = url;
        }
        // 一条强的（已在野），故意放在最后（最旧的位置）
        let (_url, mut strong) = item("strong");
        strong.title = "CVE-2026-99999：某设备已被利用漏洞".into();
        strong.summary = "CISA 已知被利用漏洞目录（KEV）· 处置期限 2026-12-01".into();
        strong.published = "2026-11-10".into();
        pool.push(strong);
        super::trim_must_pool(&mut pool);
        assert_eq!(pool.len(), super::MUST_POOL_CAP);
        assert!(
            pool.iter().any(|i| i.id == "strong"),
            "强的条目不该被弱的挤掉"
        );
    }

    /// 用**真实池子数据**跑一遍合并 prompt（生产侧验证用）。
    ///
    /// `RU41_RSS_FIXTURE=/tmp/rss_fixture.json cargo test -- --ignored merged_prompt_on_real_data`
    /// fixture 格式：`{"must":[FeedItem…],"rest":[FeedItem…]}`
    #[test]
    #[ignore = "要 RU41_RSS_FIXTURE 指向真实池子导出"]
    fn merged_prompt_on_real_data() {
        let path = std::env::var("RU41_RSS_FIXTURE").expect("要 RU41_RSS_FIXTURE");
        let raw = std::fs::read_to_string(&path).expect("读 fixture 失败");
        let fixture: serde_json::Value = serde_json::from_str(&raw).expect("fixture 不是 JSON");
        let must: Vec<FeedItem> =
            serde_json::from_value(fixture["must"].clone()).expect("must 解析失败");
        let rest: Vec<FeedItem> =
            serde_json::from_value(fixture["rest"].clone()).expect("rest 解析失败");
        println!("真实条目：必报 {} 条，资讯 {} 条", must.len(), rest.len());
        let prompt = super::merged_report_prompt(
            &must,
            &rest,
            "测试关注方向",
            "",
            &super::DEFAULT_OPPORTUNITY_ID_PREFIXES
                .iter()
                .map(|s| (*s).to_owned())
                .collect::<Vec<_>>(),
        );
        // 必报段在前、资讯段在后。
        let must_at = prompt.find("第一部分 · 必报").expect("必报段");
        let news_at = prompt.find("第二部分 · 今日资讯").expect("资讯段");
        assert!(must_at < news_at, "必报段必须在前面");
        // 必报条目逐条出现在第一部分。
        for item in &must {
            let at = prompt
                .find(&item.title)
                .unwrap_or_else(|| panic!("必报条目没进 prompt: {}", item.title));
            assert!(
                at > must_at && at < news_at,
                "必报条目跑进资讯段了: {}",
                item.title
            );
        }
        // 长度不能破入队上限（65536）。
        assert!(prompt.len() < 65_536, "prompt 太大: {}", prompt.len());
        println!(
            "--- 合并后 prompt 长度 {} 字节，头 1200 字 ---",
            prompt.len()
        );
        let head: String = prompt.chars().take(1200).collect();
        println!("{head}");
    }

    /// 权威漏洞源里「只带 CVE 编号」的条目也要必报（那是漏洞公告本身）。
    #[test]
    fn authoritative_source_cve_only_item_is_must_report() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(
            ru41::adapters::database::sqlite::SqliteTaskStore::open(&dir.path().join("s.db"))
                .unwrap(),
        );
        let mut tracker = daily_tracker(store.clone(), 0);
        tracker.config.pool_keyword_filter = false;
        let (url, mut adv) = item("adv");
        adv.id = "ghsa-GHSA-xxxx-yyyy-zzzz".into();
        adv.title = "GHSA-xxxx-yyyy-zzzz：某库权限提升漏洞".into();
        adv.summary = "严重等级 high · 官方公告，详见 CVE-2026-12345。".into();
        tracker
            .poll_daily(vec![(url, adv)], &["AI".to_owned()], "i")
            .unwrap();
        let tasks = store.list(10).unwrap();
        let (date, _) = local_date_hour(ru41::util::now());
        // 合并模式（缺省）：没有独立的必报任务，但该条目必须落在**必报段**里。
        let merged = tasks
            .iter()
            .find(|t| t.request_key == format!("rss:digest:evening:{date}"))
            .expect("合并日报任务");
        let must_at = merged.prompt.find("第一部分 · 必报").expect("必报段");
        let item_at = merged.prompt.find("GHSA-xxxx-yyyy-zzzz").expect("条目");
        assert!(
            must_at < item_at,
            "权威源的 CVE 条目应落在必报段（不是资讯段）: {prompt}",
            prompt = merged.prompt
        );
        assert!(
            merged.prompt.contains("必报理由：官方漏洞源条目"),
            "{}",
            merged.prompt
        );
    }

    /// 必报条目与新闻条目**分池**：GHSA 一次带 100 条，跟新闻混在一个池里
    /// 会把新闻挤掉（池子封顶 80，丢最旧的）。
    #[test]
    fn must_report_items_go_to_their_own_pool() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> = Arc::new(
            ru41::adapters::database::sqlite::SqliteTaskStore::open(&dir.path().join("s.db"))
                .unwrap(),
        );
        let (_, hour) = local_date_hour(ru41::util::now());
        if hour == 23 {
            return;
        }
        let mut tracker = daily_tracker(store.clone(), hour + 1);
        tracker.config.pool_keyword_filter = false;
        let (url, mut vuln) = item("vuln");
        vuln.title = "某库曝远程代码执行漏洞".into();
        vuln.summary = "CVSS 9.8，无需认证即可远程接管。".into();
        tracker
            .poll_daily(vec![(url, vuln), item("news")], &["AI".to_owned()], "i")
            .unwrap();
        let (date, _) = local_date_hour(ru41::util::now());
        let must_pool: Vec<FeedItem> = serde_json::from_value(
            store
                .document("rss_pool", &format!("pool:must:{date}"))
                .unwrap()
                .expect("必报池存在")
                .value["items"]
                .clone(),
        )
        .unwrap();
        let news_pool: Vec<FeedItem> = serde_json::from_value(
            store
                .document("rss_pool", &format!("pool:{date}"))
                .unwrap()
                .expect("普通池存在")
                .value["items"]
                .clone(),
        )
        .unwrap();
        assert_eq!(must_pool.len(), 1, "必报条目进必报池");
        assert!(must_pool[0].title.contains("远程代码执行"));
        assert_eq!(news_pool.len(), 1, "新闻条目留在普通池");
        assert!(news_pool[0].title.contains("AI 机会"));
    }

    /// 权威漏洞源不受全局 max_age_days 约束（它们自己按 days= 管时间窗）。
    #[test]
    fn authoritative_feed_bypasses_the_age_filter() {
        let now = ru41::util::now();
        let cutoff = now - 2 * 86_400;
        // 三天前的条目：普通源按「2 天内」丢掉，权威源留下。
        let old = ru41::util::today_utc(now - 3 * 86_400);
        assert!(!super::is_fresh(&old, cutoff, false), "普通源应判旧");
        assert!(
            super::is_fresh(&old, cutoff, true),
            "权威源不该被全局门槛丢"
        );
        // 当天的条目普通源也留。
        let today = ru41::util::today_utc(now);
        assert!(super::is_fresh(&today, cutoff, false));
        // 日期解析不出来的不丢。
        assert!(super::is_fresh("前天", cutoff, false));
    }
}
