//! 校园通知识别附属：QQ 消息里的班会/宣讲会通知自动抽取时间地点加入行程。
//! 规则抽取（锚点：「通知」标题行 + ⏰时间 + 🏠地点），不走 LLM。

mod extract;
mod handler;

pub use extract::{NoticeInfo, extract_notices};
pub use handler::NoticeHandler;

/// 注册表定义：notices 无定时任务，只注册 QQ 自然语言拦截。
pub static DEF: ru41::modules::ModuleDef = ru41::modules::ModuleDef {
    name: "notices",
    priority: ru41::modules::DEFAULT_PRIORITY,
    config_key: None,
    schedule: |_| Vec::new(),
    tracker: |_, _, _| Ok(None),
    qq_handlers: |config, _| {
        Ok(vec![
            std::sync::Arc::new(NoticeHandler::new(config.todo.options()?))
                as std::sync::Arc<dyn ru41::ports::commands::CommandHandler>,
        ])
    },
    image_consumers: ru41::modules::no_image_consumers,
    cli: ru41::modules::no_cli,
};

/// 宿主加载入口（插件不用 linkme，走显式注册）。
#[cfg(feature = "plugin-entry")]
#[allow(improper_ctypes_definitions)]
#[unsafe(no_mangle)]
pub extern "C" fn ru41_plugin_entry() -> ru41::plugins::PluginEntry {
    ru41::plugins::PluginEntry::new("notices", || DEF)
}
