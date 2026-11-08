use anyhow::Result;
use ru41::ports::{
    commands::{CommandContext, Reply, SimpleCommand},
    storage::TaskStore,
};
use std::sync::Arc;

use crate::service::{PagewatchConfig, WatchedPage};

/// QQ 命令 `/页面监控`：按需查看各监控页的最新状态。
///
/// 状态表不再写进 Obsidian——那是 ru41 的内部视图，不是用户的笔记。
/// 要看就在这里问。
pub struct PagewatchQqCommand {
    config: Arc<PagewatchConfig>,
}

impl PagewatchQqCommand {
    pub fn new(config: Arc<PagewatchConfig>) -> Self {
        Self { config }
    }
}

impl SimpleCommand for PagewatchQqCommand {
    fn verbs(&self) -> &'static [&'static str] {
        &["页面监控", "监控", "pagewatch"]
    }

    fn help(&self) -> &'static str {
        "`/页面监控`：查看被监控页面的最近内容变更时间"
    }

    fn help_section(&self) -> &'static str {
        "信息与学习"
    }

    async fn run(&self, arg: &str, ctx: &CommandContext<'_>) -> Result<Option<Reply>> {
        if !arg.is_empty() {
            return Ok(None);
        }
        Ok(Some(Reply::text(self.status_text(ctx.store).await?)))
    }
}

impl PagewatchQqCommand {
    /// 各监控页的最近内容变更时间。
    async fn status_text(&self, store: &Arc<dyn TaskStore>) -> Result<String> {
        let mut lines = vec!["## 📡 页面监控".to_owned(), String::new()];
        if self.config.pages.is_empty() {
            lines.push("还没配置监控页面。".to_owned());
            return Ok(lines.join("\n"));
        }
        for page in &self.config.pages {
            let doc = store.document("pagewatch_page", &page.url)?;
            let changed = doc
                .as_ref()
                .and_then(|doc| doc.value["last_changed_at"].as_i64())
                .map(crate::service::fmt_ts)
                .unwrap_or_else(|| "尚无变更记录".to_owned());
            lines.push(format!("- {}｜最近内容变更 {changed}", page.name));
        }
        Ok(lines.join("\n"))
    }
}

/// 供测试与内部复用：页面清单。
pub fn pages_of(config: &PagewatchConfig) -> &[WatchedPage] {
    &config.pages
}
