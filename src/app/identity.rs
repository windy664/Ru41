use crate::{ports::storage::TaskStore, util::now};
use anyhow::Result;
use std::sync::Arc;

/// 渠道身份绑定：把平台 openid 与个人身份关联，记录于通用 records 文档存储。
/// 绑定关系是后续审核授权与私人内容隔离的依据。
pub struct IdentityService {
    store: Arc<dyn TaskStore>,
}

impl IdentityService {
    pub fn new(store: Arc<dyn TaskStore>) -> Self {
        Self { store }
    }

    /// 校验口令并写入绑定；口令不匹配返回 Ok(false)，不泄露口令是否配置。
    pub fn bind(&self, channel: &str, openid: &str, expected: &str, given: &str) -> Result<bool> {
        if given != expected {
            return Ok(false);
        }
        self.store.put_document(
            "identity",
            &Self::key(channel, openid),
            None,
            &serde_json::json!({ "channel": channel, "openid": openid, "bound_at": now() }),
        )?;
        Ok(true)
    }

    pub fn is_bound(&self, channel: &str, openid: &str) -> Result<bool> {
        Ok(self
            .store
            .document("identity", &Self::key(channel, openid))?
            .is_some())
    }

    fn key(channel: &str, openid: &str) -> String {
        format!("{channel}:{openid}")
    }
}
