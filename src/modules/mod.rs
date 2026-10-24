pub mod blog;
pub mod chaoxing;
pub mod ecard;
pub mod homework;
pub mod jwxt;
pub mod obsidian;
pub mod papers;
pub mod planner;
pub mod projects;
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
}

impl JobSchedule {
    pub fn to_scheduled_job(&self, module: &str) -> ScheduledJob {
        ScheduledJob {
            module: module.into(),
            name: self.name.into(),
            interval_secs: Some(self.interval_secs),
            poll_command: format!("{} poll", self.name),
            run_command: format!("{} run", self.name),
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
    pub schedule: fn(&Config) -> Vec<JobSchedule>,
    /// 按 job 名构造 tracker；名字不属于本模块返回 Ok(None)。
    pub tracker: fn(&Config, Arc<dyn TaskStore>, &str) -> Result<Option<Box<dyn PollJob>>>,
    /// QQ 扩展命令；无命令返回空 Vec。
    pub qq_handlers: fn(&Config, &str) -> Result<Vec<Arc<dyn CommandHandler>>>,
}

/// 注册表：装配层遍历它构建 jobs/handlers；老模块暂时保留在
/// assembly 的手写注册里，逐个迁移过来后删除。
pub const MODULES: &[ModuleDef] = &[
    papers::DEF,
    rss::DEF,
    ecard::DEF,
    projects::DEF,
];
