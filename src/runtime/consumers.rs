//! 通用事件消费者：持久认领、续租、处理、确认；不包含渠道发送逻辑。
use crate::{
    domain::consumers::{ConsumerSpec, DeliveryFailure},
    ports::consumers::{ConsumerStore, EventHandler},
    runtime::worker::now,
};
use anyhow::{Result, ensure};
use std::{sync::Arc, time::Duration};
use tokio::sync::watch;

pub struct ConsumerWorker {
    pub store: Arc<dyn ConsumerStore>,
    pub handler: Arc<dyn EventHandler>,
    pub spec: ConsumerSpec,
    pub lease_secs: i64,
    pub timeout: Duration,
    pub retry_delay_secs: i64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ConsumeOutcome {
    Idle,
    Handled(i64),
    /// 是否已耗尽预算可通过 ConsumerState.blocked 查询。
    Failed(i64),
    LeaseLost(i64),
    Stopped,
}

impl ConsumerWorker {
    pub async fn run_once(&self, mut shutdown: watch::Receiver<bool>) -> Result<ConsumeOutcome> {
        self.spec.validate()?;
        ensure!(
            (3..=86400).contains(&self.lease_secs),
            "consumer lease must be 3..86400 seconds"
        );
        ensure!(
            !self.timeout.is_zero() && self.timeout <= Duration::from_secs(86400),
            "invalid handler timeout"
        );
        ensure!(
            (0..=3600).contains(&self.retry_delay_secs),
            "invalid retry delay"
        );
        if *shutdown.borrow() {
            return Ok(ConsumeOutcome::Stopped);
        }
        let spec = self.spec.clone();
        let lease = self.lease_secs;
        let delivery = blocking(self.store.clone(), move |store| {
            store.register_consumer(&spec)?;
            store.claim_event(&spec.name, now(), lease)
        })
        .await?;
        let Some(delivery) = delivery else {
            return Ok(ConsumeOutcome::Idle);
        };
        let id = delivery.event.id;
        let failure = {
            let work = self.handler.handle(&delivery);
            tokio::pin!(work);
            let timeout = tokio::time::sleep(self.timeout);
            tokio::pin!(timeout);
            let mut renew = tokio::time::interval(Duration::from_secs((lease / 3) as u64));
            renew.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown.changed() => break Some(DeliveryFailure::Interrupted),
                    _ = &mut timeout => break Some(DeliveryFailure::Timeout),
                    _ = renew.tick() => {
                        if *shutdown.borrow() { break Some(DeliveryFailure::Interrupted); }
                        let d = delivery.clone();
                        if !blocking(self.store.clone(), move |store| store.renew_event(&d,now(),lease)).await? {
                            return Ok(ConsumeOutcome::LeaseLost(id));
                        }
                    }
                    result = &mut work => break result.err().map(|_| DeliveryFailure::Handler),
                }
            }
        };
        // 先丢弃处理 future，再记录失败或确认；失败正文不进入公共状态。
        if let Some(reason) = failure {
            let delay =
                (self.retry_delay_secs * (1_i64 << (delivery.attempt - 1).clamp(0, 10))).min(3600);
            let recorded = blocking(self.store.clone(), move |store| {
                let timestamp = now();
                store.fail_event(&delivery, timestamp, timestamp + delay, reason)
            })
            .await?;
            Ok(if recorded {
                ConsumeOutcome::Failed(id)
            } else {
                ConsumeOutcome::LeaseLost(id)
            })
        } else {
            let recorded = blocking(self.store.clone(), move |store| {
                store.complete_event(&delivery, now())
            })
            .await?;
            Ok(if recorded {
                ConsumeOutcome::Handled(id)
            } else {
                ConsumeOutcome::LeaseLost(id)
            })
        }
    }

    /// 顺序处理；无任务、退避或阻塞时等待。存储故障返回调用方，由宿主决定重启策略。
    pub async fn run_loop(&self, mut shutdown: watch::Receiver<bool>) -> Result<()> {
        loop {
            if *shutdown.borrow() {
                return Ok(());
            }
            match self.run_once(shutdown.clone()).await? {
                ConsumeOutcome::Stopped => return Ok(()),
                ConsumeOutcome::Handled(_) => continue,
                _ => tokio::select! {
                    _ = shutdown.changed() => return Ok(()),
                    _ = tokio::time::sleep(Duration::from_millis(500)) => {},
                },
            }
        }
    }
}

async fn blocking<T: Send + 'static>(
    store: Arc<dyn ConsumerStore>,
    f: impl FnOnce(&dyn ConsumerStore) -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(move || f(store.as_ref())).await?
}
