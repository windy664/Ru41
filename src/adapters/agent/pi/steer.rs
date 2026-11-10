use crate::ports::agent::RunContext;
use anyhow::Result;
use serde_json::json;
use tokio::io::AsyncWriteExt;

/// steer 泵：把 QQ 侧写入的插话记录（steer 文档）注入运行中的 pi 进程。
/// 用户续发的消息不再打断任务——插进去，agent 干完当前步骤自然看到。
pub(super) async fn pump_steers(
    context: &RunContext,
    stdin: &mut tokio::process::ChildStdin,
) -> Result<()> {
    let Some(store) = &context.store else {
        return Ok(());
    };
    for doc in store.documents("steer", 100)? {
        if doc.value["run_id"].as_str() != Some(context.run_id.as_str())
            || doc.value["status"] != "pending"
        {
            continue;
        }
        let text = doc.value["text"].as_str().unwrap_or_default();
        if text.is_empty() {
            continue;
        }
        let command =
            json!({"id": format!("ru41-steer-{}", doc.id), "type": "steer", "message": text});
        stdin.write_all(format!("{command}\n").as_bytes()).await?;
        stdin.flush().await?;
        let mut value = doc.value.clone();
        value["status"] = json!("injected");
        store.put_document("steer", &doc.id, Some(doc.revision), &value)?;
        eprintln!("[pi] steer injected: {}", doc.id);
    }
    Ok(())
}
