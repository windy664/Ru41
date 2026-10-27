//! 博客发布管线：Obsidian `Published-已发布/` 文件夹就是发布入口。
//! 用户在 Obsidian 写完定稿、从 `Drafts-草稿/` 移入该目录并 git 同步，
//! 服务器轮询发现后经确定性校验写入博客仓库 `source/_posts/` 并推送
//! （Astro 站点自动构建）。没有中间队列目录，文件始终留在已发布目录归档。
//! 两个仓库之间只有单向通道（Vault → 博客），不做自动回环。

pub mod config;
pub mod service;

pub use config::BlogSection;
pub use service::{BlogConfig, BlogPollReport, BlogPublishTracker};
use ru41::app::obsidian::ObsidianSectionExt;

/// 注册表定义：blog 发布管线的调度清单与 tracker 闭环。
pub static DEF: ru41::modules::ModuleDef = ru41::modules::ModuleDef {
    name: "blog",
    priority: ru41::modules::DEFAULT_PRIORITY,
    config_key: Some("blog"),
    schedule: |config| {
        let blog = config
            .sources
            .get_or_default::<crate::BlogSection>("blog");
        let ready = blog.is_ready() && config.obsidian.is_ready();
        vec![ru41::modules::JobSchedule::simple(
            "blog",
            blog.interval_secs
                .unwrap_or(config::BlogSection::DEFAULT_INTERVAL_SECS),
            ready,
            (!blog.is_ready())
                .then(|| "配置 sources.blog.repo_path 后启用".into())
                .or_else(|| {
                    (!config.obsidian.is_ready())
                        .then(|| "依赖 obsidian.vault_path（待发布文件在 Vault 仓库）".into())
                }),
        )]
    },
    tracker: |config, store, name| {
        if name != "blog" {
            return Ok(None);
        }
        let blog = config
            .sources
            .get::<crate::BlogSection>("blog")?;
        anyhow::ensure!(
            config.obsidian.is_ready(),
            "sources.blog requires obsidian.vault_path (posts live in the vault repo)"
        );
        let vault_cfg = config.obsidian.resolved(None)?;
        let route = ru41::bootstrap::assembly::source_notify(config, blog.notify)?;
        let notifier = match &route {
            Some(route) => Some(ru41::bootstrap::assembly::default_notifier(
                config,
                &route.channel,
            )?),
            None => None,
        };
        let resolved = blog.resolved(route)?;
        Ok(Some(Box::new(BlogPublishTracker::new(
            store,
            ru41::bootstrap::assembly::vault_client(&vault_cfg),
            std::sync::Arc::new(ru41::adapters::vault::GitVault::new(
                resolved.repo_path.clone(),
                resolved.remote.clone(),
                resolved.branch.clone(),
            )),
            notifier,
            resolved,
        ))))
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
    ru41::plugins::PluginEntry::new("blog", || DEF)
}
