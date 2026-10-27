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
pub static DEF: ru41::modules::ModuleDef = ru41::modules::ModuleDef {
    name: "zhihuishu",
    priority: 20,
    config_key: Some("zhihuishu"),
    schedule: |config| {
        let zhihuishu = config
            .sources
            .get_or_default::<crate::ZhihuishuSection>("zhihuishu");
        // sidecar 驱动模块：没有凭证类开关，恒为启用（报告缺失时轮询自然空转）。
        vec![ru41::modules::JobSchedule::simple(
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
            .get::<crate::ZhihuishuSection>("zhihuishu")?;
        let (notifier, notify_target) =
            ru41::bootstrap::assembly::notify_pair(config, zhihuishu.notify)?;
        let vault = ru41::bootstrap::assembly::vault_if_ready(
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
                .get::<crate::ZhihuishuSection>("zhihuishu")?
                .resolved(String::new())?
                .report_path,
        ))])
    },
    image_consumers: ru41::modules::no_image_consumers,
    cli: ru41::modules::no_cli,
};

/// 宿主加载入口（插件不用 linkme，走显式注册）。
#[cfg(feature = "plugin-entry")]
#[allow(improper_ctypes_definitions)]
#[unsafe(no_mangle)]
pub extern "C" fn ru41_plugin_entry() -> ru41::plugins::PluginEntry {
    ru41::plugins::PluginEntry::new("zhihuishu", || DEF)
}
