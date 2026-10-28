use anyhow::Result;
use ru41::ports::commands::{CommandContext, CommandHandler, Reply};
use std::{future::Future, pin::Pin};

use crate::{service::deep_read_task, source::ArxivClient};

/// QQ 精读命令；由装配层注册，QQ 渠道无需认识论文模块。
pub struct PapersQqCommand {
    client: ArxivClient,
    interests: String,
    backend: String,
}

impl PapersQqCommand {
    pub fn new(client: ArxivClient, interests: String, backend: String) -> Self {
        Self {
            client,
            interests,
            backend,
        }
    }
}

impl CommandHandler for PapersQqCommand {
    fn matches(&self, verb: &str) -> bool {
        matches!(verb, "精读" | "deep" | "deepread")
    }

    fn help(&self) -> &'static str {
        "`/精读 <arxiv-id>`：精读指定论文（如 `/精读 2510.12345`）"
    }

    fn handle<'a>(
        &'a self,
        arg: &'a str,
        ctx: &'a CommandContext<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Reply>>> + Send + 'a>> {
        Box::pin(async move {
            if !is_arxiv_id(arg) {
                return Ok(None);
            }
            let route = ctx.reply_route();
            deep_read_task(
                ctx.store,
                &self.client,
                arg,
                &self.interests,
                &self.backend,
                &route,
            )
            .await
            .map(Reply::text)
            .map(Some)
        })
    }
}

/// arXiv 新格式 ID：\d{4}.\d{4,5}(v\d+)?。
fn is_arxiv_id(s: &str) -> bool {
    let (base, version) = match s.split_once('v') {
        Some((b, v)) => (b, Some(v)),
        None => (s, None),
    };
    if let Some(v) = version
        && (v.is_empty() || !v.chars().all(|c| c.is_ascii_digit()))
    {
        return false;
    }
    let Some((left, right)) = base.split_once('.') else {
        return false;
    };
    left.len() == 4
        && left.chars().all(|c| c.is_ascii_digit())
        && (4..=5).contains(&right.len())
        && right.chars().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::is_arxiv_id;

    #[test]
    fn validates_arxiv_ids_strictly() {
        assert!(is_arxiv_id("2610.18594"));
        assert!(is_arxiv_id("2610.18594v2"));
        assert!(is_arxiv_id("2510.1111"));
        assert!(!is_arxiv_id(""));
        assert!(!is_arxiv_id("abc"));
        assert!(!is_arxiv_id("123.4567"));
        assert!(!is_arxiv_id("2610.185"));
        assert!(!is_arxiv_id("2610.18594vx"));
    }
}
