//! 信息源适配器：真实 RSS/Atom 由 feed.rs 解析；无 feed 的平台以
//! 「伪 feed」接入——各源模块自描述 `DEF`（前缀 + 抓取入口），
//! rss 模块查表分发，接新源不再改 rss 内部。

pub mod competition;
pub mod feed;
pub mod github;

pub use competition::{datafountain, devpost, lablab, wqai, xfyun};

use anyhow::Result;
use feed::FeedItem;
use std::future::Future;
use std::pin::Pin;

pub type FetchFuture = Pin<Box<dyn Future<Output = Result<Vec<FeedItem>>> + Send>>;

/// 一个伪 feed 源的自描述：feed 地址里的接入前缀 + 抓取入口。
pub struct SourceDef {
    /// feed 地址前缀（如 `wqai+`），命中即路由到本源。
    pub prefix: &'static str,
    /// 抓取入口：参数为去掉前缀后的地址，产出与 RSS 条目同构的 FeedItem。
    pub fetch: fn(String) -> FetchFuture,
}

/// 伪 feed 源注册表：新源在自己的模块里加一个 DEF，这里登记一行。
pub const SOURCES: &[SourceDef] = &[
    wqai::DEF,
    devpost::DEF,
    lablab::DEF,
    datafountain::DEF,
    xfyun::DEF,
];

/// 命中某个伪 feed 前缀则返回其抓取 future；未命中返回 None（按 RSS/Atom 处理）。
pub fn fetch_pseudo(url: &str) -> Option<FetchFuture> {
    SOURCES.iter().find_map(|def| {
        url.strip_prefix(def.prefix)
            .map(|rest| (def.fetch)(rest.to_owned()))
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn registry_routes_every_prefix() {
        for (url, expect_hit) in [
            ("wqai+https://wqai.ltd/calendar/race/index.html", true),
            ("devpost+https://devpost.com/api/hackathons", true),
            ("lablab+https://lablab.ai/ai-hackathons", true),
            (
                "datafountain+https://www.datafountain.cn/competitions",
                true,
            ),
            ("xfyun+https://challenge.xfyun.cn/competition", true),
            ("https://news.ycombinator.com/rss", false),
        ] {
            assert_eq!(super::fetch_pseudo(url).is_some(), expect_hit, "{url}");
        }
    }
}
