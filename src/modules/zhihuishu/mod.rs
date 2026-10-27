pub mod config;
pub mod qq_command;
pub mod service;
pub mod source;

pub use config::ZhihuishuSection;
pub use qq_command::ZhihuishuQqCommand;
pub use service::{ZhihuishuConfig, ZhihuishuPollReport, ZhihuishuTracker};
pub use source::{
    ZhihuishuAttachment, ZhihuishuCourse, ZhihuishuReport, ZhihuishuTask, load_report,
    parse_platform_time,
};

/// 注册表定义：zhihuishu 的调度清单、tracker、QQ 命令闭环。
#[linkme::distributed_slice(super::MODULES)]
pub static DEF: super::ModuleDef = super::ModuleDef {
    name: "zhihuishu",
    priority: 20,
    config_key: Some("zhihuishu"),
    schedule: |config| {
        let zhihuishu = config
            .sources
            .get_or_default::<crate::modules::zhihuishu::ZhihuishuSection>("zhihuishu");
        // sidecar 驱动模块：没有凭证类开关，恒为启用（报告缺失时轮询自然空转）。
        vec![super::JobSchedule::simple(
            "zhihuishu",
            zhihuishu
                .interval_secs
                .unwrap_or(config::ZhihuishuSection::DEFAULT_INTERVAL_SECS),
            true,
            None,
        )]
    },
    tracker: |config, store, name| {
        if name != "zhihuishu" {
            return Ok(None);
        }
        let zhihuishu = config
            .sources
            .get::<crate::modules::zhihuishu::ZhihuishuSection>("zhihuishu")?;
        let (notifier, notify_target) =
            crate::bootstrap::assembly::notify_pair(config, zhihuishu.notify)?;
        let vault = crate::bootstrap::assembly::vault_if_ready(
            config,
            zhihuishu.resolved(notify_target.clone())?.vault_sync,
        )?;
        Ok(Some(Box::new(ZhihuishuTracker::new(
            store,
            notifier,
            zhihuishu.resolved(notify_target)?,
            config.todo.options()?,
            vault,
        ))))
    },
    qq_handlers: |config, _| {
        Ok(vec![std::sync::Arc::new(ZhihuishuQqCommand::new(
            config
                .sources
                .get::<crate::modules::zhihuishu::ZhihuishuSection>("zhihuishu")?
                .resolved(String::new())?
                .report_path,
        ))])
    },
};
