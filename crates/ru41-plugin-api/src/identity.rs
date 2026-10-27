use crate::{ports::storage::TaskStore, time::now};
use anyhow::Result;
use std::sync::Arc;

/// 渠道身份绑定：把平台 openid 与个人身份关联，记录于通用 records 文档存储。
/// 绑定关系是后续审核授权与私人内容隔离的依据。
pub struct IdentityService {
    store: Arc<dyn TaskStore>,
}

/// 绑定失败限流策略：窗口期内最多 5 次，超出锁定至窗口结束。
/// 锁定期间不校验口令（正确口令也先拒绝），避免口令正确性被当枚举探针。
pub const BIND_MAX_ATTEMPTS: u32 = 5;
pub const BIND_WINDOW_SECS: i64 = 600;

/// 绑定结果：成功、口令错误、或窗口内失败过多被锁定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindOutcome {
    Bound,
    Rejected,
    Locked,
}

#[derive(Default, Clone, Copy)]
struct BindAttempts {
    count: u32,
    window_start: i64,
}

impl IdentityService {
    pub fn new(store: Arc<dyn TaskStore>) -> Self {
        Self { store }
    }

    /// 校验口令并写入绑定；口令错误会计入窗口限流，锁定后窗口过期自动重置。
    pub fn bind(
        &self,
        channel: &str,
        openid: &str,
        expected: &str,
        given: &str,
        at: i64,
    ) -> Result<BindOutcome> {
        let key = Self::attempts_key(channel, openid);
        let doc = self.store.document("bind_attempts", &key)?;
        let mut attempts = doc
            .as_ref()
            .map(|d| BindAttempts {
                count: d.value["count"].as_u64().unwrap_or(0) as u32,
                window_start: d.value["window_start"].as_i64().unwrap_or(0),
            })
            .unwrap_or_default();
        // 窗口过期即重置。
        if at - attempts.window_start >= BIND_WINDOW_SECS {
            attempts = BindAttempts {
                count: 0,
                window_start: at,
            };
        }
        if attempts.count >= BIND_MAX_ATTEMPTS {
            return Ok(BindOutcome::Locked);
        }
        if given != expected {
            attempts.count += 1;
            self.store.put_document(
                "bind_attempts",
                &key,
                doc.as_ref().map(|d| d.revision),
                &serde_json::json!({
                    "count": attempts.count,
                    "window_start": attempts.window_start,
                }),
            )?;
            return Ok(BindOutcome::Rejected);
        }
        // 成功：清零失败记录并写入绑定。
        if doc.is_some() {
            self.store.put_document(
                "bind_attempts",
                &key,
                doc.as_ref().map(|d| d.revision),
                &serde_json::json!({ "count": 0, "window_start": at }),
            )?;
        }
        let existing = self
            .store
            .document("identity", &Self::key(channel, openid))?;
        self.store.put_document(
            "identity",
            &Self::key(channel, openid),
            existing.map(|d| d.revision),
            &serde_json::json!({ "channel": channel, "openid": openid, "bound_at": now() }),
        )?;
        Ok(BindOutcome::Bound)
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

    fn attempts_key(channel: &str, openid: &str) -> String {
        format!("{channel}:{openid}")
    }
}
