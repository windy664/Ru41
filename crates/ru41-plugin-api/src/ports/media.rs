//! 渠道无关的富媒体类型；宿主渠道适配器与插件命令共用，
//! 放在 plugin-api 是因为 CommandContext 需要把消息附件透传给插件。

/// 富媒体类型（渠道无关）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Video,
    Voice,
    File,
}

/// 富媒体来源：http(s) URL 渠道直接引用，其余按本地文件路径上传。
#[derive(Clone, Debug)]
pub struct MediaRef {
    pub kind: MediaKind,
    pub source: String,
    /// 文件类附件的展示名；缺省取路径/URL 末段。
    pub filename: Option<String>,
    /// 渠道媒体 CDN 的下载认证头值（如 QQ 的 `QQBot <token>`）。
    /// 宿主入站时快照；插件下载附件时应作为 `Authorization` 头带上，
    /// 否则部分渠道（QQ 多媒体 CDN）会拒绝请求。
    pub auth: Option<String>,
}

impl MediaRef {
    pub fn new(kind: MediaKind, source: impl Into<String>) -> Self {
        Self {
            kind,
            source: source.into(),
            filename: None,
            auth: None,
        }
    }

    /// 附加 CDN 下载认证头值（渠道适配器在入站时调用）。
    pub fn with_auth(mut self, auth: impl Into<String>) -> Self {
        self.auth = Some(auth.into());
        self
    }

    pub fn is_remote(&self) -> bool {
        self.source.starts_with("http://") || self.source.starts_with("https://")
    }

    pub fn display_name(&self) -> &str {
        self.filename.as_deref().unwrap_or_else(|| {
            self.source
                .rsplit(['/', '\\'])
                .next()
                .filter(|s| !s.is_empty())
                .unwrap_or("附件")
        })
    }
}
