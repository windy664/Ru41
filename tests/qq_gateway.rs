use ru41::channels::qq::gateway::{
    GatewayEvent, close_forces_reidentify, close_is_fatal, heartbeat_payload, heartbeat_stalled,
    identify_payload, parse_dispatch, resume_payload,
};
use serde_json::json;
use std::time::{Duration, Instant};

#[test]
fn parses_group_at_message() {
    let frame = json!({
        "id": "evt-1", "op": 0, "s": 42, "t": "GROUP_AT_MESSAGE_CREATE",
        "d": {
            "id": "msg-abc",
            "content": " 帮我总结一下今天的收件箱 ",
            "group_openid": "G123",
            "author": { "member_openid": "U456" }
        }
    });
    assert_eq!(
        parse_dispatch(&frame),
        GatewayEvent::GroupMessage {
            msg_id: "msg-abc".into(),
            content: "帮我总结一下今天的收件箱".into(),
            group_openid: "G123".into(),
            author_openid: "U456".into(),
        }
    );
}

#[test]
fn parses_c2c_message() {
    let frame = json!({
        "op": 0, "t": "C2C_MESSAGE_CREATE",
        "d": { "id": "msg-1", "content": "hello", "author": { "user_openid": "U1" } }
    });
    assert_eq!(
        parse_dispatch(&frame),
        GatewayEvent::C2cMessage {
            msg_id: "msg-1".into(),
            content: "hello".into(),
            user_openid: "U1".into(),
        }
    );
}

#[test]
fn parses_ready_and_resumed() {
    let ready = json!({"op": 0, "t": "READY", "d": {"session_id": "s-1"}});
    assert_eq!(
        parse_dispatch(&ready),
        GatewayEvent::Ready {
            session_id: "s-1".into()
        }
    );
    assert_eq!(
        parse_dispatch(&json!({"op": 0, "t": "RESUMED"})),
        GatewayEvent::Resumed
    );
}

#[test]
fn empty_or_incomplete_messages_are_ignored() {
    for frame in [
        json!({"op": 0, "t": "GROUP_AT_MESSAGE_CREATE", "d": {"id": "m", "content": "  ", "group_openid": "G"}}),
        json!({"op": 0, "t": "GROUP_AT_MESSAGE_CREATE", "d": {"content": "hi", "group_openid": "G"}}),
        json!({"op": 0, "t": "C2C_MESSAGE_CREATE", "d": {"id": "m", "content": "hi"}}),
        json!({"op": 0, "t": "INTERACTION_CREATE", "d": {}}),
    ] {
        assert!(matches!(parse_dispatch(&frame), GatewayEvent::Other(_)));
    }
}

#[test]
fn payloads_match_qq_protocol() {
    let identify = identify_payload("token-1");
    assert_eq!(identify["op"], 2);
    assert_eq!(identify["d"]["token"], "QQBot token-1");
    assert_eq!(identify["d"]["intents"], (1 << 25) | (1 << 26));
    assert_eq!(identify["d"]["shard"], json!([0, 1]));
    let resume = resume_payload("token-1", "s-1", 7);
    assert_eq!(resume["op"], 6);
    assert_eq!(resume["d"]["session_id"], "s-1");
    assert_eq!(resume["d"]["seq"], 7);
    assert_eq!(heartbeat_payload(Some(7))["d"], 7);
    assert!(heartbeat_payload(None)["d"].is_null());
}

#[test]
fn close_codes_drive_reconnect_policy() {
    assert!(close_forces_reidentify(4009));
    assert!(close_forces_reidentify(4002));
    assert!(close_is_fatal(4014));
    assert!(close_is_fatal(4914));
    assert!(!close_is_fatal(4008));
    assert!(!close_forces_reidentify(1000));
}

#[test]
fn request_key_is_derived_from_msg_id() {
    assert_eq!(ru41::channels::qq::msg_request_key("msg-1"), "qq:msg:msg-1");
}

#[test]
fn heartbeat_stall_detection() {
    let interval = Duration::from_secs(30);
    let t0 = Instant::now();
    assert!(!heartbeat_stalled(t0, t0, interval));
    // 阈值是 2 个周期 + 10s 宽限：70s 内仍视为存活。
    assert!(!heartbeat_stalled(
        t0,
        t0 + Duration::from_secs(69),
        interval
    ));
    assert!(heartbeat_stalled(
        t0,
        t0 + Duration::from_secs(71),
        interval
    ));
}
