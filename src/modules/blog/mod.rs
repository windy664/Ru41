//! 博客发布管线：Obsidian `Published-已发布/` 文件夹就是发布入口。
//! 用户在 Obsidian 写完定稿、从 `Drafts-草稿/` 移入该目录并 git 同步，
//! 服务器轮询发现后经确定性校验写入博客仓库 `source/_posts/` 并推送
//! （Astro 站点自动构建）。没有中间队列目录，文件始终留在已发布目录归档。
//! 两个仓库之间只有单向通道（Vault → 博客），不做自动回环。

pub mod config;
pub mod service;

pub use config::BlogSection;
pub use service::{BlogConfig, BlogPollReport, BlogPublishTracker};

/// 注册表定义：blog 发布管线的调度清单与 tracker 闭环。
#[linkme::distributed_slice(super::MODULES)]
pub static DEF: super::ModuleDef = super::ModuleDef {
    name: "blog",
    priority: super::DEFAULT_PRIORITY,
    config_key: Some("blog"),
    schedule: |config| {
        let blog = config
            .sources
            .get_or_default::<crate::modules::blog::BlogSection>("blog");
        let ready = blog.is_ready() && config.obsidian.is_ready();
        vec![super::JobSchedule::simple(
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
            .get::<crate::modules::blog::BlogSection>("blog")?;
        anyhow::ensure!(
            config.obsidian.is_ready(),
            "sources.blog requires obsidian.vault_path (posts live in the vault repo)"
        );
        let vault_cfg = config.obsidian.resolved(None)?;
        let route = crate::bootstrap::assembly::source_notify(config, blog.notify)?;
        let notifier = match &route {
            Some(route) => Some(crate::bootstrap::assembly::default_notifier(
                config,
                &route.channel,
            )?),
            None => None,
        };
        let resolved = blog.resolved(route)?;
        Ok(Some(Box::new(BlogPublishTracker::new(
            store,
            crate::bootstrap::assembly::vault_client(&vault_cfg),
            std::sync::Arc::new(crate::adapters::vault::GitVault::new(
                resolved.repo_path.clone(),
                resolved.remote.clone(),
                resolved.branch.clone(),
            )),
            notifier,
            resolved,
        ))))
    },
    qq_handlers: |_, _| Ok(Vec::new()),
    qq_image_consumers: |_| Ok(Vec::new()),
};
