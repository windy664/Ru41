//! Obsidian 附属的入口层：Vault 核心服务在 `app::obsidian`（端口 `ports::vault`），
//! 这里只保留同步/整理调度，业务逻辑不落地。

use ru41::app::obsidian::ObsidianSectionExt;

/// 注册表定义：obsidian 同步服务（含知识库索引挂载）。
pub static DEF: ru41::modules::ModuleDef = ru41::modules::ModuleDef {
    name: "obsidian",
    priority: ru41::modules::DEFAULT_PRIORITY,
    config_key: None,
    schedule: |config| {
        let ready = config.obsidian.is_ready();
        vec![ru41::modules::JobSchedule::simple(
            "obsidian",
            config
                .obsidian
                .interval_secs
                .unwrap_or(ru41::bootstrap::config::ObsidianSection::DEFAULT_INTERVAL_SECS),
            ready,
            (!ready).then(|| "需 [obsidian] vault_path 已配置".into()),
        )]
    },
    tracker: |config, store, name| {
        if name != "obsidian" {
            return Ok(None);
        }
        let obsidian = &config.obsidian;
        let route = ru41::bootstrap::assembly::source_notify(config, obsidian.notify)?;
        let notifier = match &route {
            Some(route) => Some(ru41::bootstrap::assembly::default_notifier(
                config,
                &route.channel,
            )?),
            None => None,
        };
        let resolved = obsidian.resolved_with_provider(route, &config.agent)?;
        let service = ru41::app::obsidian::ObsidianService::new(
            store,
            ru41::bootstrap::assembly::vault_client(&resolved),
            notifier,
            resolved,
        );
        // 知识库索引进插件副本就失去 local-embed（插件不带 ONNX）：
        // 接线失败降级为无知识模式，不影响 git 同步本体；索引用宿主 CLI
        // `ru41 knowledge index`（镜像宿主二进制带 fastembed）。
        let service = match ru41::bootstrap::assembly::knowledge_service(config) {
            Ok(Some(knowledge)) => service.with_knowledge(knowledge),
            Ok(None) => service,
            Err(error) => {
                eprintln!("[obsidian] 知识库接线跳过（插件内无 local-embed）：{error:#}");
                service
            }
        };
        Ok(Some(Box::new(service)))
    },
    qq_handlers: |_, _| Ok(Vec::new()),
    image_consumers: ru41::modules::no_image_consumers,
    cli: ru41::modules::no_cli,
};

/// 宿主加载入口（插件不用 linkme，走显式注册）。
/// feature 门控：被其他插件依赖时不导出，编 .so 必须 --features plugin-entry。
#[cfg(feature = "plugin-entry")]
#[allow(improper_ctypes_definitions)]
#[unsafe(no_mangle)]
pub extern "C" fn ru41_plugin_entry() -> ru41::plugins::PluginEntry {
    ru41::plugins::PluginEntry::new("obsidian", || DEF)
}
