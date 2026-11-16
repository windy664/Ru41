//! QQ 命令 `/校园跑`：查学期进度、最近记录与账号状态。
//!
//! 只读。不提供「帮我跑一次」——提交类动作以后单独评估（要人确认）。

use crate::client::CampusrunClient;
use crate::config::CampusrunSection;
use crate::service::fetch_markdown;
use anyhow::Result;
use ru41::ports::commands::{CommandContext, Reply, SimpleCommand};

/// 校园跑查询命令。
pub struct CampusrunQqCommand {
    section: CampusrunSection,
}

impl CampusrunQqCommand {
    pub fn new(section: CampusrunSection) -> Self {
        Self { section }
    }
}

impl SimpleCommand for CampusrunQqCommand {
    fn verbs(&self) -> &'static [&'static str] {
        &["校园跑", "跑步", "campusrun"]
    }

    fn help(&self) -> &'static str {
        "`/校园跑 [进度|记录|状态]`：查校园跑学期进度、最近记录与账号状态（需绑定身份，仅单聊）"
    }

    fn help_section(&self) -> &'static str {
        "校园生活"
    }

    async fn run(&self, arg: &str, ctx: &CommandContext<'_>) -> Result<Option<Reply>> {
        // 只认明确的参数；其余不拦，交给 agent（skill 会走 CLI 查）。
        let what = match arg.trim() {
            "" | "进度" | "progress" | "status" => "status",
            "记录" | "records" => "records",
            "状态" | "风控" | "cheat" => "cheat",
            "全部" | "all" => "all",
            _ => return Ok(None),
        };
        if let Some(reply) = ctx.require_private("查询校园跑进度", "校园跑记录属于个人信息")?
        {
            return Ok(Some(reply.into()));
        }
        let section = self.section.clone();
        let text = ru41::plugins::bridge(async move {
            let client =
                CampusrunClient::new(&section).map_err(|error| anyhow::anyhow!("{error}"))?;
            fetch_markdown(&client, what, 5)
                .await
                .map_err(|error| anyhow::anyhow!("{error}"))
        })
        .await?;
        Ok(Some(Reply::text(text)))
    }
}
