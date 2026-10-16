use ru41::ingress::schedule::run_periodic;
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

/// 跑一个 30ms 间隔的计数轮询循环，100ms 后 shutdown，返回执行轮数。
async fn count_rounds(fail: bool) -> usize {
    let count = Arc::new(AtomicUsize::new(0));
    let counter = count.clone();
    let (shutdown, receiver) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(async move {
        run_periodic(
            "test",
            Duration::from_millis(30),
            move || {
                let count = counter.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    if fail {
                        anyhow::bail!("boom");
                    }
                    Ok(serde_json::json!({"ok": true}))
                }
            },
            receiver,
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    shutdown.send(true).unwrap();
    handle.await.unwrap().unwrap();
    count.load(Ordering::SeqCst)
}

#[tokio::test]
async fn periodic_loop_polls_until_shutdown() {
    // 立即执行一轮 + 若干间隔轮，随后因 shutdown 退出。
    assert!(count_rounds(false).await >= 2);
}

#[tokio::test]
async fn periodic_loop_survives_poll_failures() {
    assert!(count_rounds(true).await >= 2);
}
