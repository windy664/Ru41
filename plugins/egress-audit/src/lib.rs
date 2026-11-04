//! P3+ 出站审计（observe-only，privacy-llm-boundary.md）：
//! 聚合宿主机采集器写入的 mihomo 连接日志（/data/egress/conns-*.jsonl，
//! 只含 容器/源 IP/目标域名/命中规则，无内容），白名单外的新增出站域名
//! 当日首报；只观察不阻断。容器流量经 compose 注入的 HTTP(S)_PROXY 进
//! mihomo（GEOIP CN 直连语义不变），这是观察层成立的前提。

use anyhow::{Context, Result};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
    sync::Arc,
};

/// 出站审计白名单外的域名单日首报去重状态。
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct AlertState {
    date: String,
    alerted: Vec<String>,
}

/// [sources.egress_audit] 配置。
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EgressAuditSection {
    /// 是否经默认通知出口提醒白名单外新域名；默认 true。
    pub notify: Option<bool>,
    /// 聚合轮询间隔（秒），默认 6 小时；日报始终覆盖最近 24 小时。
    pub interval_secs: Option<u64>,
    /// 追加的白名单（支持 *.example.com 通配）；内置默认白名单见 DEFAULT_ALLOWLIST。
    pub allowlist: Option<Vec<String>>,
    /// 连接日志目录；默认 /data/egress（宿主采集器写入，容器只读消费）。
    pub data_dir: Option<PathBuf>,
}

impl EgressAuditSection {
    pub const DEFAULT_INTERVAL_SECS: u64 = 6 * 3600;

    pub fn is_ready(&self) -> bool {
        true // 数据目录缺失时 poll 优雅降级，无需配置门槛。
    }
}

/// 内置白名单：已知业务出口（国内抓取、LLM API、代码托管、文献源）。
/// 命中规则：精确相等，或 `*.example.com` 通配任意子域（含裸域）。
pub const DEFAULT_ALLOWLIST: &[&str] = &[
    // LLM API
    "open.bigmodel.cn",
    "api.zhipuai.cn",
    "api.deepseek.com",
    "api.openai.com",
    "api.anthropic.com",
    // 渠道
    "*.qq.com",
    "q.qq.com",
    "open.feishu.cn",
    "*.feishu.cn",
    "*.larksuite.com",
    // 代码托管与镜像
    "*.github.com",
    "github.com",
    "*.githubusercontent.com",
    "ghcr.io",
    "*.ghcr.io",
    "crates.io",
    "static.crates.io",
    "registry.npmjs.org",
    "cdn.jsdelivr.net",
    "unpkg.com",
    // 校园与抓取源（国内直连语义下出口不变）
    "*.edu.cn",
    "*.libsp.cn",
    "*.chaoxing.com",
    "reserve.chaoxing.com",
    "*.zhihuishu.com",
    "*.zhipin.com",
    "*.teachmate.cn",
    "*.xfyun.cn",
    "*.myqcloud.com",
    "*.qpic.cn",
    "*.gtimg.cn",
    "*.tencent.com",
    // 文献与信息源
    "*.arxiv.org",
    "*.semanticscholar.org",
    "api.crossref.org",
    "api.openalex.org",
    "*.youtube.com",
    "*.ytimg.com",
    "*.googlevideo.com",
    "*.gstatic.com",
    "*.ggpht.com",
    "*.reddit.com",
    "www.qbitai.com",
    "www.ithome.com",
    "huggingface.co",
    "*.huggingface.co",
    "hf-mirror.com",
    "www.chaspark.com",
];

fn domain_allowed(domain: &str, extra: &[String]) -> bool {
    let d = domain.trim_end_matches('.').to_ascii_lowercase();
    DEFAULT_ALLOWLIST
        .iter()
        .map(|s| *s)
        .chain(extra.iter().map(|s| s.as_str()))
        .any(|entry| {
            let e = entry.trim_end_matches('.').to_ascii_lowercase();
            if let Some(suffix) = e.strip_prefix("*.") {
                d == suffix || d.ends_with(&format!(".{suffix}"))
            } else {
                d == e
            }
        })
}

/// 单日连接的按容器聚合视图：容器 → (域名 → 请求数)。
pub fn aggregate<S: AsRef<str>>(lines: &[S]) -> BTreeMap<String, BTreeMap<String, u64>> {
    let mut out: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    for line in lines {
        let Ok(rec) = serde_json::from_str::<serde_json::Value>(line.as_ref()) else {
            continue;
        };
        let container = rec["container"].as_str().unwrap_or("unknown");
        let dst = rec["dst"].as_str().unwrap_or("");
        let domain = match dst.rsplit_once(':') {
            // 端口段是纯数字才算 host:port；IPv6 裸地址不拆。
            Some((host, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => {
                host.to_owned()
            }
            _ => dst.to_owned(),
        };
        if domain.is_empty() {
            continue;
        }
        *out
            .entry(container.to_owned())
            .or_default()
            .entry(domain)
            .or_insert(0) += 1;
    }
    out
}

/// 白名单外的域名集合。
pub fn unknown_domains(
    aggregated: &BTreeMap<String, BTreeMap<String, u64>>,
    extra_allowlist: &[String],
) -> BTreeMap<String, Vec<(String, u64)>> {
    let mut out = BTreeMap::new();
    for (container, domains) in aggregated {
        let mut unknown: Vec<(String, u64)> = domains
            .iter()
            .filter(|(d, _)| !domain_allowed(d, extra_allowlist))
            .map(|(d, n)| (d.clone(), *n))
            .collect();
        unknown.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        if !unknown.is_empty() {
            out.insert(container.clone(), unknown);
        }
    }
    out
}

pub struct EgressAuditConfig {
    pub data_dir: PathBuf,
    pub extra_allowlist: Vec<String>,
    pub notify_target: String,
    pub interval_secs: u64,
}

pub struct EgressAuditTracker {
    config: EgressAuditConfig,
    notifier: Option<Arc<dyn ru41::ports::channel::NotifyChannel>>,
}

impl EgressAuditTracker {
    /// 聚合最近 24h 连接 → 写日报 → 白名单外新域名当日首报。
    async fn poll_once(&self) -> Result<serde_json::Value> {
        let today = chrono_today();
        let lines = read_recent_lines(&self.config.data_dir, &today)?;
        let aggregated = aggregate(&lines);
        let unknown = unknown_domains(&aggregated, &self.config.extra_allowlist);

        // 日报始终写（覆盖式），供人工查阅与后续 QQ 查询命令消费。
        let digest = render_digest(&aggregated, &unknown, &today);
        let digest_path = self.config.data_dir.join("digest-latest.md");
        if let Err(e) = std::fs::write(&digest_path, digest) {
            // 数据目录不可写不应打断轮询，只记日志。
            eprintln!("[egress-audit] digest 写入失败 {digest_path:?}: {e}");
        }

        // 白名单外新域名当日首报（跨容器合并去重，state 随日期翻转）。
        let mut new_domains: Vec<String> = Vec::new();
        let state = self.load_state(&today);
        for domains in unknown.values() {
            for (d, _) in domains {
                if !state.alerted.iter().any(|a| a == d) {
                    new_domains.push(d.clone());
                }
            }
        }
        new_domains.sort();
        new_domains.dedup();
        if !new_domains.is_empty() {
            if let Some(notifier) = &self.notifier {
                let preview: Vec<String> =
                    new_domains.iter().take(8).cloned().collect();
                let more = new_domains.len() - preview.len();
                let text = format!(
                    "🌐 出站审计：今日发现 {} 个白名单外域名{}：{}\n日报见 vault 外 /data/egress/digest-latest.md",
                    new_domains.len(),
                    if more > 0 { format!("（另 {more} 个）") } else { String::new() },
                    preview.join("、"),
                );
                notifier
                    .send(&self.config.notify_target, None, &text)
                    .await?;
            }
            self.save_state(&today, &new_domains)?;
        }
        Ok(serde_json::json!({
            "date": today,
            "containers": aggregated.len(),
            "unknown_containers": unknown.len(),
            "new_alerted": new_domains,
        }))
    }

    fn state_path(&self) -> PathBuf {
        self.config.data_dir.join("audit-state.json")
    }

    fn load_state(&self, today: &str) -> AlertState {
        let state: AlertState = std::fs::read(self.state_path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        if state.date == today {
            state
        } else {
            AlertState {
                date: today.to_owned(),
                alerted: Vec::new(),
            }
        }
    }

    fn save_state(&self, today: &str, new_domains: &[String]) -> Result<()> {
        let mut state = self.load_state(today);
        state.date = today.to_owned();
        state.alerted.extend(new_domains.iter().cloned());
        std::fs::write(
            self.state_path(),
            serde_json::to_vec(&state).context("serialize alert state")?,
        )
        .with_context(|| format!("write {:?}", self.state_path()))
    }
}

fn chrono_today() -> String {
    // 避免为取日期引入 chrono：state 用本机日期，宿主采集器与容器时区一致（UTC）。
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = secs / 86400;
    // civil-from-days（Howard Hinnant 算法），UTC 即可：跨日 ±8h 误差对「当日去重」无碍。
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// 读最近两天（今天 + 昨天）的 JSONL，返回行文本列表。
fn read_recent_lines(data_dir: &std::path::Path, today: &str) -> Result<Vec<String>> {
    let yesterday = prev_date(today);
    let mut lines = Vec::new();
    for date in [yesterday, today.to_owned()] {
        let path = data_dir.join(format!("conns-{date}.jsonl"));
        if let Ok(content) = std::fs::read_to_string(&path) {
            lines.extend(content.lines().map(str::to_owned));
        }
    }
    Ok(lines)
}

fn prev_date(today: &str) -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let yesterday = secs.saturating_sub(86400) / 86400;
    let _ = today;
    let z = yesterday as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

fn render_digest(
    aggregated: &BTreeMap<String, BTreeMap<String, u64>>,
    unknown: &BTreeMap<String, Vec<(String, u64)>>,
    date: &str,
) -> String {
    let mut md = format!("# 出站审计日报 {date}\n\n覆盖最近 24h；⚠ 为白名单外域名。\n\n");
    for (container, domains) in aggregated {
        md.push_str(&format!("## {container}\n\n| 域名 | 次数 |\n|---|---|\n"));
        let unknown_set: HashSet<&String> = unknown
            .get(container)
            .map(|v| v.iter().map(|(d, _)| d).collect())
            .unwrap_or_default();
        let mut list: Vec<(&String, &u64)> = domains.iter().collect();
        list.sort_by(|a, b| b.1.cmp(a.1));
        for (d, n) in list.iter().take(50) {
            let flag = if unknown_set.contains(d) { "⚠ " } else { "" };
            md.push_str(&format!("| {flag}{d} | {n} |\n"));
        }
        if domains.len() > 50 {
            md.push_str(&format!("| …（其余 {} 个） | |\n", domains.len() - 50));
        }
        md.push('\n');
    }
    md
}

impl ru41::ingress::schedule::PollJob for EgressAuditTracker {
    fn job_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.config.interval_secs)
    }

    fn poll_json(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = anyhow::Result<serde_json::Value>> + Send + '_>,
    > {
        let owned = self;
        Box::pin(async move {
            let value = owned.poll_once().await?;
            Ok(value)
        })
    }
}

/// 注册表定义：调度清单与 tracker 构造在本模块内闭环。
pub static DEF: ru41::modules::ModuleDef = ru41::modules::ModuleDef {
    name: "egress-audit",
    priority: ru41::modules::DEFAULT_PRIORITY,
    config_key: Some("egress_audit"),
    schedule: |config| {
        let section = config
            .sources
            .get_or_default::<EgressAuditSection>("egress_audit");
        vec![ru41::modules::JobSchedule {
            name: "egress-audit",
            interval_secs: section
                .interval_secs
                .unwrap_or(EgressAuditSection::DEFAULT_INTERVAL_SECS),
            enabled: section.is_ready(),
            disabled_reason: None,
            poll_command: None,
            run_command: Some("schedule run egress-audit"),
        }]
    },
    tracker: |config, _store, name| {
        if name != "egress-audit" {
            return Ok(None);
        }
        let section = config.sources.get::<EgressAuditSection>("egress_audit")?;
        let (notifier, notify_target) =
            ru41::bootstrap::assembly::notify_pair(config, section.notify)?;
        Ok(Some(Box::new(EgressAuditTracker {
            config: EgressAuditConfig {
                data_dir: section
                    .data_dir
                    .clone()
                    .unwrap_or_else(|| PathBuf::from("/data/egress")),
                extra_allowlist: section.allowlist.clone().unwrap_or_default(),
                notify_target,
                interval_secs: section
                    .interval_secs
                    .unwrap_or(EgressAuditSection::DEFAULT_INTERVAL_SECS),
            },
            notifier,
        })))
    },
    qq_handlers: |_, _| Ok(Vec::new()),
    image_consumers: ru41::modules::no_image_consumers,
    cli: ru41::modules::no_cli,
};

/// 宿主加载入口（插件不用 linkme，走显式注册）。
#[cfg(feature = "plugin-entry")]
#[allow(improper_ctypes_definitions)]
#[unsafe(no_mangle)]
pub extern "C" fn ru41_plugin_entry() -> ru41::plugins::PluginEntry {
    ru41::plugins::PluginEntry::new("egress-audit", || DEF)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregates_by_container_and_domain() {
        let lines = vec![
            r#"{"ts":"t1","net":"TCP","container":"ru41-qq","src":"172.20.0.2:1","dst":"api.github.com:443","rule":"","proxy":""}"#,
            r#"{"ts":"t2","net":"TCP","container":"ru41-qq","src":"172.20.0.2:2","dst":"api.github.com:443","rule":"","proxy":""}"#,
            r#"{"ts":"t3","net":"TCP","container":"ru41-rss","src":"172.17.0.5:3","dst":"1.2.3.4:443","rule":"","proxy":""}"#,
            "not json",
            // 裸 IPv6 目标不拆 host:port。
            r#"{"ts":"t4","net":"TCP","container":"ru41-x","src":"172.20.0.9:9","dst":"2606:4700::6810:84e5"}"#,
        ];
        let refs: Vec<&str> = lines.iter().map(|s| *s).collect();
        let agg = aggregate(&refs);
        assert_eq!(agg["ru41-qq"]["api.github.com"], 2);
        assert_eq!(agg["ru41-rss"]["1.2.3.4"], 1);
        assert_eq!(agg.len(), 3); // 非 json 行跳过；源 IP 过滤由采集器负责，这里全量聚合。
    }

    #[test]
    fn unknown_filters_allowlist_and_wildcards() {
        let mut agg = BTreeMap::new();
        let mut domains = BTreeMap::new();
        domains.insert("api.github.com".to_owned(), 3u64);
        domains.insert("evil.example.com".to_owned(), 1u64);
        domains.insert("raw.githubusercontent.com".to_owned(), 2u64);
        agg.insert("ru41-worker".to_owned(), domains);
        let unknown = unknown_domains(&agg, &[]);
        let names: Vec<&str> = unknown["ru41-worker"].iter().map(|(d, _)| d.as_str()).collect();
        assert_eq!(names, vec!["evil.example.com"]);
        // config 追加白名单生效。
        let unknown2 = unknown_domains(
            &agg,
            &["evil.example.com".to_owned()].into_iter().collect::<Vec<_>>(),
        );
        assert!(unknown2.is_empty());
    }

    #[test]
    fn digest_lists_unknown_first_with_flag() {
        let mut agg = BTreeMap::new();
        let mut domains = BTreeMap::new();
        domains.insert("a.example".to_owned(), 5u64);
        agg.insert("ru41-qq".to_owned(), domains);
        let mut unknown = BTreeMap::new();
        unknown.insert("ru41-qq".to_owned(), vec![("a.example".to_owned(), 5u64)]);
        let md = render_digest(&agg, &unknown, "2026-11-04");
        assert!(md.contains("⚠ a.example"));
        assert!(md.contains("出站审计日报 2026-11-04"));
    }
}
