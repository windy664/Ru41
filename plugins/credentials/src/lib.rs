//! 凭据托管附属：QQ 侧存/查名单/删除（/存凭据 /凭据 /删凭据）。
//! 真值使用走核心 CLI `ru41 secret get <名称>`——shell 内联展开、执行期生效，
//! 不进模型上下文（社区共识 SecretRef 模式：agent 知道凭据存在但永远看不到真值）。

mod qq_command;
pub use qq_command::CredentialQqCommand;

/// 注册表定义：无轮询任务，只有 QQ 命令。
pub static DEF: ru41::modules::ModuleDef = ru41::modules::ModuleDef {
    name: "credentials",
    priority: ru41::modules::DEFAULT_PRIORITY,
    config_key: None,
    schedule: |_| vec![],
    tracker: |_, _, _| Ok(None),
    qq_handlers: |_, _| Ok(vec![std::sync::Arc::new(CredentialQqCommand)]),
    image_consumers: ru41::modules::no_image_consumers,
    cli: ru41::modules::no_cli,
};

/// 宿主加载入口（插件不用 linkme，走显式注册）。
/// feature 门控：被其他插件依赖时不导出，编 .so 必须 --features plugin-entry。
#[cfg(feature = "plugin-entry")]
#[allow(improper_ctypes_definitions)]
#[unsafe(no_mangle)]
pub extern "C" fn ru41_plugin_entry() -> ru41::plugins::PluginEntry {
    ru41::plugins::PluginEntry::new("credentials", || DEF)
}
