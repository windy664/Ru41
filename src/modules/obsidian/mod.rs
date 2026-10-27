//! Obsidian 附属的入口层：Vault 核心服务在 `app::obsidian`（端口 `ports::vault`），
//! 这里只保留 QQ 快速记录命令与归档技能，业务逻辑不落地。

pub mod qq_command;

pub use qq_command::ObsidianQqCommand;
use crate::app::obsidian::ObsidianSectionExt;

/// 注册表定义：obsidian 同步服务（含知识库索引挂载）与 /记一下 命令闭环。
#[linkme::distributed_slice(super::MODULES)]
pub static DEF: super::ModuleDef = super::ModuleDef {
    name: "obsidian",
    priority: super::DEFAULT_PRIORITY,
    config_key: None,
    schedule: |config| {
        let ready = config.obsidian.is_ready();
        vec![super::JobSchedule::simple(
            "obsidian",
            config
                .obsidian
                .interval_secs
                .unwrap_or(crate::bootstrap::config::ObsidianSection::DEFAULT_INTERVAL_SECS),
            ready,
            (!ready).then(|| "需 [obsidian] vault_path 已配置".into()),
        )]
    },
    tracker: |config, store, name| {
        if name != "obsidian" {
            return Ok(None);
        }
        let obsidian = &config.obsidian;
        let route = crate::bootstrap::assembly::source_notify(config, obsidian.notify)?;
        let notifier = match &route {
            Some(route) => Some(crate::bootstrap::assembly::default_notifier(
                config,
                &route.channel,
            )?),
            None => None,
        };
        let resolved = obsidian.resolved(route)?;
        let service = crate::app::obsidian::ObsidianService::new(
            store,
            crate::bootstrap::assembly::vault_client(&resolved),
            notifier,
            resolved,
        );
        let service = match crate::bootstrap::assembly::knowledge_service(config)? {
            Some(knowledge) => service.with_knowledge(knowledge),
            None => service,
        };
        // 待办视图投影：todo 配置缺失/非法时跳过视图，不影响 git 闭环本体。
        let service = match config.todo.options() {
            Ok(options) => service.with_todo_view(options),
            Err(_) => service,
        };
        Ok(Some(Box::new(service)))
    },
    qq_handlers: |config, _| {
        // 未配置 vault_path 时不注册 /记一下，避免半配置状态下写空气。
        if !config.obsidian.is_ready() {
            return Ok(Vec::new());
        }
        let resolved = config.obsidian.resolved(None)?;
        Ok(vec![std::sync::Arc::new(ObsidianQqCommand::new(
            crate::bootstrap::assembly::vault_client(&resolved),
            resolved.inbox_dir.clone(),
            config.todo.options()?.offset_seconds,
        ))])
    },
    image_consumers: super::no_image_consumers,
    cli: super::no_cli,
};
