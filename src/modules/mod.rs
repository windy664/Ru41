pub mod chaoxing;
pub mod jwxt;
pub mod obsidian;
pub mod planner;
pub mod teachermate;
pub mod youtube;

/// 附属注册表：新模块只需①写自己目录 ②这里加一行——调度清单、
/// tracker 装配、QQ 命令注册全部数据驱动，不再碰 main.rs/assembly.rs。
use crate::bootstrap::config::Config;
use crate::ingress::schedule::{PollJob, ScheduledJob};
use crate::ports::commands::CommandHandler;
use crate::ports::storage::TaskStore;
use anyhow::Result;
use std::sync::Arc;

/// 一个模块对调度层声明的 job（不构造 tracker，未配置时给出原因）。
pub struct JobSchedule {
    /// 任务名，也是 `schedule run` 的参数。
    pub name: &'static str,
    pub interval_secs: u64,
    pub enabled: bool,
    pub disabled_reason: Option<String>,
    /// 单轮入口覆盖（默认 `{name} poll`）；如 chaoxing-work 用 `chaoxing work`。
    pub poll_command: Option<&'static str>,
    /// 长驻入口覆盖（默认 `{name} run`）；如 class-remind 用 `schedule run class-remind`。
    pub run_command: Option<&'static str>,
}

impl JobSchedule {
    pub fn simple(
        name: &'static str,
        interval_secs: u64,
        enabled: bool,
        disabled_reason: Option<String>,
    ) -> Self {
        Self {
            name,
            interval_secs,
            enabled,
            disabled_reason,
            poll_command: None,
            run_command: None,
        }
    }

    pub fn to_scheduled_job(&self, module: &str) -> ScheduledJob {
        ScheduledJob {
            module: module.into(),
            name: self.name.into(),
            interval_secs: Some(self.interval_secs),
            poll_command: self
                .poll_command
                .map(str::to_owned)
                .unwrap_or_else(|| format!("{} poll", self.name)),
            run_command: self
                .run_command
                .map(str::to_owned)
                .unwrap_or_else(|| format!("{} run", self.name)),
            enabled: self.enabled,
            notes: self.disabled_reason.clone(),
        }
    }
}

/// 附属模块的自描述：装配自己的轮询 job 与 QQ 命令。
/// 构造时配置未就绪应返回 Ok(None)/空 Vec（模块静默缺席），而不是报错。
/// Copy：动态插件按值返回 DEF，宿主 Box::leak 成 'static。
#[derive(Clone, Copy)]
pub struct ModuleDef {
    pub name: &'static str,
    /// 注册顺序（QQ 命令 first-match-wins 的消费序，小者先行）。
    /// 默认 DEFAULT_PRIORITY；跨平台聚合类命令（如 homework 必须先于
    /// 单平台 zhihuishu）显式给更小值。
    pub priority: u8,
    /// 该模块在 `[sources]` 下的配置段名；None 表示无独立配置段
    /// （如 homework 聚合其他平台、obsidian 走顶层 [obsidian]）。
    /// 用于 [sources] 未知键告警；papers 的配置段是 "arxiv"（历史名）。
    pub config_key: Option<&'static str>,
    /// 调度清单（schedule list 展示用，不构造 tracker）。
    pub schedule: ScheduleFn,
    /// 按 job 名构造 tracker；名字不属于本模块返回 Ok(None)。
    pub tracker: TrackerFn,
    /// QQ 扩展命令；无命令返回空 Vec。
    pub qq_handlers: QqHandlersFn,
    /// QQ 图片消费者（如学习通二维码签到）；无则用 no_image_consumers。
    pub image_consumers: ImageConsumersFn,
    /// 专属 CLI 子命令（poll/run 之外，如 youtube prep / chaoxing sign / jwxt login）；
    /// 入参为模块名之后的原始参数，输出 JSON。无则用 no_cli。
    pub cli: CliFn,
}

/// ModuleDef 的装配入口签名（抽类型别名压平复杂度）。
pub type ScheduleFn = fn(&Config) -> Vec<JobSchedule>;
pub type TrackerFn = fn(&Config, Arc<dyn TaskStore>, &str) -> Result<Option<Box<dyn PollJob>>>;
pub type QqHandlersFn = fn(&Config, &str) -> Result<Vec<Arc<dyn CommandHandler>>>;
pub type ImageConsumersFn = fn(&Config) -> Result<Vec<Arc<dyn crate::ports::commands::ImageConsumer>>>;
/// 专属 CLI 输出（异步 fn 指针；实现内如需 reactor 请过 plugins::bridge）。
pub type CliFn = fn(
    &Config,
    &[String],
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<Option<serde_json::Value>>> + Send>,
>;

/// 无图片消费者的默认实现。
pub fn no_image_consumers(
    _: &Config,
) -> Result<Vec<Arc<dyn crate::ports::commands::ImageConsumer>>> {
    Ok(Vec::new())
}

/// 无专属 CLI 的默认实现：返回 None，由入口侧报"不支持"。
pub fn no_cli(
    _: &Config,
    _: &[String],
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<Option<serde_json::Value>>> + Send>,
> {
    Box::pin(async { Ok(None) })
}

/// 绝大多数模块的注册顺序：不显式关心 first-match-wins 语义时用此值。
pub const DEFAULT_PRIORITY: u8 = 100;

/// 分布式注册表：各模块在自己的 mod.rs 里用
/// `#[linkme::distributed_slice(MODULES)] pub static DEF: ModuleDef = ...`
/// 自注册，新增附属不再编辑本文件的注册清单。
/// 注意：linkme 的遍历顺序不保证稳定，消费方一律走 `sorted()`。
#[linkme::distributed_slice]
pub static MODULES: [ModuleDef];

/// 按 (priority, name) 稳定排序后的注册表视图。
pub fn sorted() -> Vec<&'static ModuleDef> {
    let mut defs: Vec<&'static ModuleDef> = MODULES.iter().collect();
    defs.extend(crate::plugins::external_defs());
    defs.sort_by(|a, b| a.priority.cmp(&b.priority).then_with(|| a.name.cmp(b.name)));
    defs
}
