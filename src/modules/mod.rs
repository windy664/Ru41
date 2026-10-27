pub mod blog;
pub mod chaoxing;
pub mod competitions;
pub mod ecard;
pub mod homework;
pub mod jwxt;
pub mod jobs;
pub mod obsidian;
pub mod pagewatch;
pub mod papers;
pub mod planner;
pub mod programs;
pub mod projects;
pub mod research;
pub mod rss;
pub mod teachermate;
pub mod youtube;
pub mod zhihuishu;

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
pub struct ModuleDef {
    pub name: &'static str,
    /// 调度清单（schedule list 展示用，不构造 tracker）。
    pub schedule: ScheduleFn,
    /// 按 job 名构造 tracker；名字不属于本模块返回 Ok(None)。
    pub tracker: TrackerFn,
    /// QQ 扩展命令；无命令返回空 Vec。
    pub qq_handlers: QqHandlersFn,
}

/// ModuleDef 的三个装配入口签名（抽类型别名压平复杂度）。
pub type ScheduleFn = fn(&Config) -> Vec<JobSchedule>;
pub type TrackerFn = fn(&Config, Arc<dyn TaskStore>, &str) -> Result<Option<Box<dyn PollJob>>>;
pub type QqHandlersFn = fn(&Config, &str) -> Result<Vec<Arc<dyn CommandHandler>>>;

/// 注册表：装配层遍历它构建 jobs/handlers；老模块暂时保留在
/// assembly 的手写注册里，逐个迁移过来后删除。
pub const MODULES: &[ModuleDef] = &[
    papers::DEF,
    pagewatch::DEF,
    rss::DEF,
    ecard::DEF,
    projects::DEF,
    youtube::DEF,
    chaoxing::DEF,
    teachermate::DEF,
    jwxt::DEF,
    // 跨平台聚合 /作业 必须先于 zhihuishu 注册（first-match-wins）。
    homework::DEF,
    zhihuishu::DEF,
    obsidian::DEF,
    blog::DEF,
    planner::DEF,
    competitions::DEF,
    jobs::DEF,
    programs::DEF,
    research::DEF,
];
