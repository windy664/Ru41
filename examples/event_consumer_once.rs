//! 单次消费示例；打印事件元数据，不发送外部消息。
use ru41::{
    adapters::database::sqlite::SqliteTaskStore,
    domain::consumers::{ConsumerSpec, EventDelivery},
    ports::consumers::EventHandler,
    runtime::consumers::ConsumerWorker,
};
use std::{future::Future, io::Write, path::Path, pin::Pin, sync::Arc, time::Duration};

struct PrintMetadata;
impl EventHandler for PrintMetadata {
    fn handle<'a>(
        &'a self,
        delivery: &'a EventDelivery,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'a>> {
        Box::pin(async move {
            // 示例输出可能重复；实际副作用需使用此键持久去重或传给支持幂等的下游。
            writeln!(
                std::io::stdout(),
                "{}",
                serde_json::json!({"key":delivery.idempotency_key(),"event":delivery.event})
            )?;
            Ok(())
        })
    }
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    anyhow::ensure!(
        args.len() == 3,
        "usage: event_consumer_once <sqlite-path> <consumer-name>"
    );
    let worker = ConsumerWorker {
        store: Arc::new(SqliteTaskStore::open(Path::new(&args[1]))?),
        handler: Arc::new(PrintMetadata),
        spec: ConsumerSpec {
            name: args[2].clone(),
            start_after: 0,
            max_attempts: 3,
        },
        lease_secs: 30,
        timeout: Duration::from_secs(30),
        retry_delay_secs: 5,
    };
    let (_sender, receiver) = tokio::sync::watch::channel(false);
    eprintln!("{:?}", worker.run_once(receiver).await?);
    Ok(())
}
