use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt, stream::SplitSink};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::Message};

type WsSink = SplitSink<WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>, Message>;

/// 网关写操作统一带 5s 超时：半开连接上卡住的 send 不能拖停整个 select 循环。
async fn send_with_timeout(sink: &mut WsSink, message: Message) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(5), sink.send(message))
        .await
        .context("QQ gateway send timed out")??;
    Ok(())
}

pub const OP_DISPATCH: u8 = 0;
pub const OP_HEARTBEAT: u8 = 1;
pub const OP_IDENTIFY: u8 = 2;
pub const OP_RESUME: u8 = 6;
pub const OP_RECONNECT: u8 = 7;
pub const OP_INVALID_SESSION: u8 = 9;
pub const OP_HELLO: u8 = 10;
pub const OP_HEARTBEAT_ACK: u8 = 11;

/// 群消息 + C2C（1<<25）与按钮交互（1<<26）。
pub const INTENTS: u32 = (1 << 25) | (1 << 26);

/// 一次 DISPATCH 事件解析结果；只关心消息与会话事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayEvent {
    Ready {
        session_id: String,
    },
    Resumed,
    GroupMessage {
        msg_id: String,
        content: String,
        group_openid: String,
        author_openid: String,
    },
    C2cMessage {
        msg_id: String,
        content: String,
        user_openid: String,
    },
    Other(String),
}

/// RESUME 所需的会话状态。
#[derive(Default)]
pub struct GatewaySession {
    pub session_id: Option<String>,
    pub last_seq: Option<i64>,
}

pub fn identify_payload(token: &str) -> Value {
    json!({
        "op": OP_IDENTIFY,
        "d": { "token": format!("QQBot {token}"), "intents": INTENTS, "shard": [0, 1] },
    })
}

pub fn resume_payload(token: &str, session_id: &str, seq: i64) -> Value {
    json!({
        "op": OP_RESUME,
        "d": { "token": format!("QQBot {token}"), "session_id": session_id, "seq": seq },
    })
}

pub fn heartbeat_payload(last_seq: Option<i64>) -> Value {
    json!({ "op": OP_HEARTBEAT, "d": last_seq })
}

/// 解析一条 DISPATCH（op 0）帧；非消息事件返回 Other。
pub fn parse_dispatch(frame: &Value) -> GatewayEvent {
    let t = frame.get("t").and_then(Value::as_str).unwrap_or_default();
    let d = frame.get("d").cloned().unwrap_or(Value::Null);
    match t {
        "READY" => GatewayEvent::Ready {
            session_id: d
                .get("session_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        },
        "RESUMED" => GatewayEvent::Resumed,
        "GROUP_AT_MESSAGE_CREATE" => {
            let msg_id = d.get("id").and_then(Value::as_str).unwrap_or_default();
            let content = d
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_owned();
            let group_openid = d
                .get("group_openid")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let author_openid = d
                .pointer("/author/member_openid")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if msg_id.is_empty() || content.is_empty() || group_openid.is_empty() {
                return GatewayEvent::Other(t.to_owned());
            }
            GatewayEvent::GroupMessage {
                msg_id: msg_id.to_owned(),
                content,
                group_openid: group_openid.to_owned(),
                author_openid: author_openid.to_owned(),
            }
        }
        "C2C_MESSAGE_CREATE" => {
            let msg_id = d.get("id").and_then(Value::as_str).unwrap_or_default();
            let content = d
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_owned();
            let user_openid = d
                .pointer("/author/user_openid")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if msg_id.is_empty() || content.is_empty() || user_openid.is_empty() {
                return GatewayEvent::Other(t.to_owned());
            }
            GatewayEvent::C2cMessage {
                msg_id: msg_id.to_owned(),
                content,
                user_openid: user_openid.to_owned(),
            }
        }
        other => GatewayEvent::Other(other.to_owned()),
    }
}

/// 断线重连时应重新 IDENTIFY 而不是 RESUME 的关闭码。
pub fn close_forces_reidentify(code: u16) -> bool {
    matches!(code, 4002 | 4004 | 4009)
}

/// 权限不足等不应重连的关闭码。
pub fn close_is_fatal(code: u16) -> bool {
    matches!(code, 4014 | 4914 | 4915)
}

/// 心跳活性判定：最近 ACK 超过 2 个心跳周期 + 10s 视为连接静默死亡，应主动断开重连。
/// 半开连接上 TCP 超时可能要几十分钟，靠 ACK 探测才能在分钟级发现。
pub fn heartbeat_stalled(last_ack: Instant, now: Instant, interval: Duration) -> bool {
    now.saturating_duration_since(last_ack) > interval.saturating_mul(2) + Duration::from_secs(10)
}

/// 运行一条 WebSocket 会话：HELLO→IDENTIFY/RESUME→心跳→事件经 events 送出。
/// 返回 Err 表示连接中断，由调用方决定是否带 session 重连。
pub async fn run(
    url: &str,
    token: &str,
    session: &mut GatewaySession,
    events: mpsc::Sender<GatewayEvent>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let (ws, _) = tokio::time::timeout(
        Duration::from_secs(10),
        tokio_tungstenite::connect_async(url),
    )
    .await
    .context("QQ gateway connect timed out")?
    .context("cannot connect QQ gateway WebSocket")?;
    let (mut sink, mut stream) = ws.split();
    let hello = tokio::time::timeout(Duration::from_secs(10), stream.next())
        .await
        .context("QQ gateway HELLO timed out")?
        .context("QQ gateway closed before HELLO")??;
    let hello: Value =
        serde_json::from_str(hello.to_text().context("QQ gateway HELLO was not text")?)?;
    if hello.get("op").and_then(Value::as_u64) != Some(OP_HELLO as u64) {
        bail!("QQ gateway first frame was not HELLO: {hello}");
    }
    let heartbeat_ms = hello
        .pointer("/d/heartbeat_interval")
        .and_then(Value::as_u64)
        .unwrap_or(41250);
    let login = match (&session.session_id, session.last_seq) {
        (Some(id), Some(seq)) => resume_payload(token, id, seq),
        _ => identify_payload(token),
    };
    send_with_timeout(&mut sink, Message::text(login.to_string())).await?;
    // 心跳略提前，避免临界点超时。
    let heartbeat_interval = Duration::from_millis(heartbeat_ms.max(10_000) - 5_000);
    let mut heartbeat = tokio::time::interval(heartbeat_interval);
    heartbeat.tick().await;
    let mut last_ack = Instant::now();
    loop {
        tokio::select! {
            biased;
            _ = shutdown.changed() => {
                let _ = send_with_timeout(&mut sink, Message::Close(None)).await;
                return Ok(());
            }
            _ = heartbeat.tick() => {
                if heartbeat_stalled(last_ack, Instant::now(), heartbeat_interval) {
                    bail!("QQ gateway heartbeat stalled, reconnecting");
                }
                send_with_timeout(&mut sink, Message::text(heartbeat_payload(session.last_seq).to_string())).await?;
            }
            frame = stream.next() => {
                let frame = frame.context("QQ gateway stream ended")??;
                let Message::Text(text) = frame else { continue };
                let frame: Value = serde_json::from_str(&text).context("invalid QQ gateway frame")?;
                if let Some(seq) = frame.get("s").and_then(Value::as_i64) {
                    session.last_seq = Some(seq);
                }
                match frame.get("op").and_then(Value::as_u64) {
                    Some(op) if op == OP_DISPATCH as u64 => {
                        let event = parse_dispatch(&frame);
                        match &event {
                            GatewayEvent::Ready { session_id } => session.session_id = Some(session_id.clone()),
                            GatewayEvent::Resumed => {}
                            _ => {}
                        }
                        if events.send(event).await.is_err() {
                            return Ok(());
                        }
                    }
                    Some(op) if op == OP_HEARTBEAT_ACK as u64 => {
                        last_ack = Instant::now();
                    }
                    Some(op) if op == OP_RECONNECT as u64 => bail!("QQ gateway requested reconnect"),
                    Some(op) if op == OP_INVALID_SESSION as u64 => {
                        if frame.get("d").and_then(Value::as_bool) != Some(true) {
                            session.session_id = None;
                            session.last_seq = None;
                        }
                        bail!("QQ gateway invalidated the session");
                    }
                    _ => {}
                }
            }
        }
    }
}
