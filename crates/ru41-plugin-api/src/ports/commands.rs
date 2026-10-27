//! 渠道扩展命令的公共扩展点：业务模块实现 `CommandHandler`，装配层注册，
//! 渠道适配器只负责收发，不认识具体业务。渠道字面量由适配器注入
//! `CommandContext::reply_channel`，模块内不得手写渠道名。

use crate::identity::IdentityService;
use anyhow::Result;
use std::{future::Future, pin::Pin, sync::Arc};

use super::storage::TaskStore;

/// 命令回复：markdown 文本 + 可选的快捷按钮。
/// 按钮动作统一为「代用户发出一条指令文本」（如 `/任务 3`），
/// 渠道适配器负责把 markup 翻译成平台键盘格式；不支持键盘的渠道忽略 markup 只发文本。
#[derive(Clone, Debug)]
pub struct Reply {
    pub text: String,
    pub markup: Option<ReplyMarkup>,
}

impl Reply {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            markup: None,
        }
    }

    pub fn with_markup(text: impl Into<String>, markup: ReplyMarkup) -> Self {
        Self {
            text: text.into(),
            markup: Some(markup),
        }
    }
}

impl From<String> for Reply {
    fn from(text: String) -> Self {
        Reply::text(text)
    }
}

/// 键盘：行数组，每行若干按钮。渠道层裁剪到平台上限。
#[derive(Clone, Debug)]
pub struct ReplyMarkup {
    pub rows: Vec<Vec<MarkupButton>>,
}

impl ReplyMarkup {
    pub fn new() -> Self {
        Self { rows: Vec::new() }
    }

    pub fn row(mut self, buttons: Vec<MarkupButton>) -> Self {
        self.rows.push(buttons);
        self
    }

    pub fn push_row(&mut self, buttons: Vec<MarkupButton>) {
        self.rows.push(buttons);
    }

    pub fn is_empty(&self) -> bool {
        self.rows.iter().all(|row| row.is_empty())
    }
}

impl Default for ReplyMarkup {
    fn default() -> Self {
        Self::new()
    }
}

/// 单个按钮：展示文案 + 点击后发送的指令文本。
#[derive(Clone, Debug)]
pub struct MarkupButton {
    pub label: String,
    pub command: String,
}

impl MarkupButton {
    pub fn new(label: impl Into<String>, command: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            command: command.into(),
        }
    }
}

/// 图片消息消费者：业务模块自行下载图片并判断归属，返回 Some(回执文本)
/// 表示该消息已按图片语义处理完毕（不再进命令/Agent 通道）；返回 None
/// 表示与己无关，消息继续走原有通道。渠道适配器本身不认识具体业务。
pub trait ImageConsumer: Send + Sync {
    fn handle_image<'a>(
        &'a self,
        image_url: &'a str,
    ) -> Pin<Box<dyn Future<Output = Option<String>> + Send + 'a>>;
}

/// 渠道扩展命令：由业务模块实现、装配层注册，渠道适配器本身不认识具体业务。
/// 返回 Ok(None) 表示不拦截（如参数非法），消息继续按自然语言入队给 Agent。
pub trait CommandHandler: Send + Sync {
    /// 是否匹配命令词；verb 已转为小写。
    fn matches(&self, verb: &str) -> bool;
    /// 单行帮助文本。
    fn help(&self) -> &'static str;
    fn handle<'a>(
        &'a self,
        arg: &'a str,
        ctx: &'a CommandContext<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Reply>>> + Send + 'a>>;
    /// 一个 handler 支持多个命令词时，可覆盖此方法来区分 verb；默认保持旧接口行为。
    fn handle_command<'a>(
        &'a self,
        _verb: &'a str,
        arg: &'a str,
        ctx: &'a CommandContext<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Reply>>> + Send + 'a>> {
        self.handle(arg, ctx)
    }
    /// 可选地处理自然语言消息；默认不拦截，消息继续入队给 Agent。
    /// 只有模块已完成渠道、身份和隐私校验后，才应向 Agent 注入私人数据。
    fn handle_message<'a>(
        &'a self,
        _content: &'a str,
        _ctx: &'a CommandContext<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Reply>>> + Send + 'a>> {
        Box::pin(async { Ok(None) })
    }
}

/// 命令执行上下文：渠道适配器只负责收发，业务判断集中在命令实现里。
pub struct CommandContext<'a> {
    pub store: &'a Arc<dyn TaskStore>,
    /// 未配置口令时绑定功能关闭。
    pub bind_secret: Option<&'a str>,
    /// 当前会话的通知 target（如 group:{openid} 或 user:{openid}）。
    pub target: &'a str,
    /// 当前消息 ID，扩展命令可用它登记终态被动回复路由。
    pub msg_id: &'a str,
    /// 发消息用户的 openid，用于绑定状态判定。
    pub author_openid: &'a str,
    /// 是否单聊；只有单聊且已绑定时才聚合该用户跨会话的任务。
    pub is_c2c: bool,
    /// 消息里的图片附件 URL（如二维码照片）；纯文本消息为 None。
    pub image_url: Option<&'a str>,
    /// 渠道标识（如 "qq"），由渠道适配器注入，模块内不得手写。
    pub reply_channel: &'a str,
}

impl CommandContext<'_> {
    /// 当前会话的终态被动回复路由。渠道字面量与 target/msg_id 语义只存在于
    /// 渠道层，业务模块构造 NotifyRoute 必须走这里，不在模块内手写渠道名。
    pub fn reply_route(&self) -> crate::domain::NotifyRoute {
        crate::domain::NotifyRoute {
            channel: self.reply_channel.to_owned(),
            target: self.target.to_owned(),
            msg_id: Some(self.msg_id.to_owned()),
        }
    }

    /// 私人数据双闸：先身份绑定，再限定单聊。返回 Some(回复) 表示已拦截，
    /// 返回 Ok(None) 表示放行。`action` 用于绑定提示（如 "查询水电"），
    /// `privacy_note` 用于单聊提示（如 "水电属于个人信息"）。
    pub fn require_private(&self, action: &str, privacy_note: &str) -> Result<Option<String>> {
        let identity = IdentityService::new(self.store.clone());
        if !identity.is_bound(self.reply_channel, self.author_openid)? {
            return Ok(Some(format!(
                "## 需要身份绑定\n\n请先发送 `绑定 <口令>`，再{action}。"
            )));
        }
        if !self.is_c2c {
            return Ok(Some(format!(
                "## 请单聊使用\n\n{privacy_note}，群聊不展示。"
            )));
        }
        Ok(None)
    }
}

/// 渠道消息的任务请求键，保证同一 msg_id 只入队一次（渠道适配器与扩展命令共用）。
pub fn msg_request_key(msg_id: &str) -> String {
    format!("qq:msg:{msg_id}")
}
