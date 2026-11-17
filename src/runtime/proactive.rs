//! 主动推送的按渠道投递：提醒类消息也双投，且**按渠道分别记账**。
//!
//! 背景：dispatcher 的 fanout 只作用于「任务结果」（日报等走任务队列的）。
//! 提醒类消息（到点提醒、行程提醒、错过的课程提醒）是直接调渠道发的，
//! 绕过了 dispatcher，于是只到主渠道——用户以为「双网关推送」，
//! 实际提醒类消息根本没双投。
//!
//! 这里复用 dispatcher 同一套记账方式：`delivery` 文档里存每个渠道的送达状态，
//! 某个渠道失败重试时，已送达的渠道不重发。

use crate::{
    domain::NotifyRoute,
    ports::channel::{NotifyChannel, ProactiveChannel},
    ports::storage::TaskStore,
};
use anyhow::Result;
use std::{future::Future, pin::Pin, sync::Arc};

/// 主动推送投递器：主渠道 + `[notify].fanout` 里的并行渠道。
#[derive(Clone)]
pub struct ProactiveDispatcher {
    store: Arc<dyn TaskStore>,
    channels: Vec<Arc<dyn NotifyChannel>>,
    fanout: Vec<String>,
}

impl ProactiveDispatcher {
    pub fn new(
        store: Arc<dyn TaskStore>,
        channels: Vec<Arc<dyn NotifyChannel>>,
        fanout: Vec<String>,
    ) -> Self {
        Self {
            store,
            channels,
            fanout,
        }
    }

    /// 目标渠道清单：主渠道（带自己的目标）+ 并行渠道（用各自的 default 目标）。
    fn targets(&self, route: &NotifyRoute) -> Vec<(String, String)> {
        let mut targets = vec![(route.channel.clone(), route.target.clone())];
        for extra in &self.fanout {
            if extra == &route.channel {
                continue;
            }
            if self.channels.iter().any(|c| c.channel_id() == extra) {
                targets.push((extra.clone(), "default".to_owned()));
            }
        }
        targets
    }
}

impl ProactiveChannel for ProactiveDispatcher {
    fn deliver<'a>(
        &'a self,
        route: &'a NotifyRoute,
        delivery_id: &'a str,
        text: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<bool>> + Send + 'a>> {
        Box::pin(async move { self.deliver_inner(route, delivery_id, text).await })
    }
}

impl ProactiveDispatcher {
    async fn deliver_inner(
        &self,
        route: &NotifyRoute,
        delivery_id: &str,
        text: &str,
    ) -> Result<bool> {
        let targets = self.targets(route);
        // 记账文档与 dispatcher 同一套（id 前缀区分）：proactive:<delivery_id>
        let id = format!("proactive:{delivery_id}");
        let current = self.store.document("delivery", &id)?;
        let now = crate::runtime::worker::now();
        if let Some(doc) = &current {
            if doc.value["status"] == "sent" {
                return Ok(true);
            }
            if doc.value["next_at"].as_i64().unwrap_or(0) > now {
                return Ok(false);
            }
        }
        let tries = current
            .as_ref()
            .and_then(|doc| doc.value["tries"].as_u64())
            .unwrap_or(0)
            + 1;
        // 重试时保留上一轮已送达的渠道：一个渠道失败不该让别的渠道重发。
        let mut delivered: serde_json::Map<String, serde_json::Value> = current
            .as_ref()
            .and_then(|doc| doc.value["sent"].as_object().cloned())
            .unwrap_or_default();
        let mut record = serde_json::json!({
            "delivery_id": delivery_id,
            "channel": route.channel,
            "target": route.target,
            "status": "sending",
            "tries": tries,
            "next_at": now + 60,
            "sent": serde_json::Value::Object(delivered.clone()),
        });
        // 首写用当前存储版本做 CAS（没有就是插入）；抢不到说明别人在写，下一轮再来。
        if !self.store.put_document(
            "delivery",
            &id,
            current.as_ref().map(|doc| doc.revision),
            &record,
        )? {
            return Ok(false);
        }
        let mut revision = current.as_ref().map(|doc| doc.revision + 1).unwrap_or(1);
        for (channel_name, target) in &targets {
            if delivered.contains_key(channel_name) {
                continue;
            }
            let Some(channel) = self
                .channels
                .iter()
                .find(|c| c.channel_id() == channel_name)
            else {
                // 容器里没这个渠道（比如没带凭据）：跳过，不算失败。
                continue;
            };
            if let Err(error) = channel.send(target, None, text).await {
                // 记账：已送达的渠道保留，下一轮只补这个渠道。
                record["status"] = "retry_scheduled".into();
                record["next_at"] = (now + (1_i64 << tries.min(10)).min(3600)).into();
                record["sent"] = serde_json::Value::Object(delivered.clone());
                self.store
                    .put_document("delivery", &id, Some(revision), &record)?;
                return Err(error);
            }
            delivered.insert(channel_name.clone(), serde_json::json!(true));
            record["sent"] = serde_json::Value::Object(delivered.clone());
            self.store
                .put_document("delivery", &id, Some(revision), &record)?;
            revision += 1;
        }
        record["status"] = "sent".into();
        record["next_at"] = serde_json::json!(0);
        self.store
            .put_document("delivery", &id, Some(revision), &record)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::database::sqlite::SqliteTaskStore;
    use std::sync::Mutex;

    /// 记录收到的消息，按渠道分桶。
    #[derive(Default)]
    struct Recorder {
        sent: Mutex<Vec<(String, String)>>,
        fail_channels: Mutex<Vec<String>>,
    }

    impl Recorder {
        fn count(&self, channel: &str) -> usize {
            self.sent
                .lock()
                .unwrap()
                .iter()
                .filter(|(name, _)| name == channel)
                .count()
        }
    }

    struct FakeChannel {
        id: String,
        recorder: Arc<Recorder>,
    }

    impl NotifyChannel for FakeChannel {
        fn channel_id(&self) -> &str {
            &self.id
        }

        fn send<'a>(
            &'a self,
            _target: &'a str,
            _msg_id: Option<&'a str>,
            text: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
            let id = self.id.clone();
            let recorder = self.recorder.clone();
            let text = text.to_owned();
            Box::pin(async move {
                if recorder.fail_channels.lock().unwrap().contains(&id) {
                    anyhow::bail!("{id} is down");
                }
                recorder.sent.lock().unwrap().push((id, text));
                Ok(())
            })
        }
    }

    fn dispatcher(
        recorder: &Arc<Recorder>,
        channels: Vec<&str>,
        fail: Vec<&str>,
    ) -> (ProactiveDispatcher, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> =
            Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
        recorder.fail_channels.lock().unwrap().extend(
            fail.into_iter()
                .map(|name| name.to_owned())
                .collect::<Vec<String>>(),
        );
        let channels: Vec<Arc<dyn NotifyChannel>> = channels
            .into_iter()
            .map(|id| {
                Arc::new(FakeChannel {
                    id: id.to_owned(),
                    recorder: recorder.clone(),
                }) as Arc<dyn NotifyChannel>
            })
            .collect();
        (
            ProactiveDispatcher::new(store, channels, vec!["telegram".to_owned()]),
            dir,
        )
    }

    fn route() -> NotifyRoute {
        NotifyRoute {
            channel: "qq".into(),
            target: "user:U1".into(),
            msg_id: None,
        }
    }

    #[tokio::test]
    async fn delivers_to_primary_and_fanout_channels() {
        let recorder = Arc::new(Recorder::default());
        let (dispatcher, _dir) = dispatcher(&recorder, vec!["qq", "telegram"], vec![]);
        let delivered = dispatcher
            .deliver(&route(), "event:100", "课程提醒")
            .await
            .unwrap();
        assert!(delivered);
        assert_eq!(recorder.count("qq"), 1);
        assert_eq!(recorder.count("telegram"), 1);
    }

    #[tokio::test]
    async fn failed_channel_is_retried_without_resending_the_delivered_one() {
        let recorder = Arc::new(Recorder::default());
        let (dispatcher, _dir) = dispatcher(&recorder, vec!["qq", "telegram"], vec!["telegram"]);
        // 第一轮：telegram 失败 → 整体失败，qq 已送达。
        let failed = dispatcher.deliver(&route(), "event:200", "课程提醒").await;
        assert!(failed.is_err());
        assert_eq!(recorder.count("qq"), 1);
        assert_eq!(recorder.count("telegram"), 0);

        // 渠道修好（不再失败）。
        recorder.fail_channels.lock().unwrap().clear();
        // 修好后重试：先过掉退避窗（next_at），再投。
        let id = "proactive:event:200";
        let doc = dispatcher.store.document("delivery", id).unwrap().unwrap();
        let mut value = doc.value.clone();
        value["next_at"] = serde_json::json!(0);
        dispatcher
            .store
            .put_document("delivery", id, Some(doc.revision), &value)
            .unwrap();

        let delivered = dispatcher
            .deliver(&route(), "event:200", "课程提醒")
            .await
            .unwrap();
        assert!(delivered);
        assert_eq!(recorder.count("qq"), 1, "已送达的渠道不该重发");
        assert_eq!(recorder.count("telegram"), 1);
    }

    #[tokio::test]
    async fn same_delivery_id_is_not_sent_twice() {
        let recorder = Arc::new(Recorder::default());
        let (dispatcher, _dir) = dispatcher(&recorder, vec!["qq", "telegram"], vec![]);
        assert!(
            dispatcher
                .deliver(&route(), "event:300", "x")
                .await
                .unwrap()
        );
        // 同一 id 再来一次（例如下一轮循环）：已送达，不再重发。
        assert!(
            dispatcher
                .deliver(&route(), "event:300", "x")
                .await
                .unwrap()
        );
        assert_eq!(recorder.count("qq"), 1);
        assert_eq!(recorder.count("telegram"), 1);
    }
}
