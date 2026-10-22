use ru41::channels::qq::gateway::{
    AttachmentKind, GatewayEvent, MessageAttachment, close_forces_reidentify, close_is_fatal,
    heartbeat_payload, heartbeat_stalled, identify_payload, op_forces_reidentify, parse_dispatch,
    resume_payload,
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
            attachments: vec![],
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
            image_url: None,
            attachments: vec![],
        }
    );
}

/// 纯图片消息（content 为空）不该被丢：首个图片附件 URL 进入事件，
/// 供「二维码照片 → 自动签」链路使用。
#[test]
fn parses_c2c_image_message() {
    let frame = json!({
        "op": 0, "t": "C2C_MESSAGE_CREATE",
        "d": {
            "id": "msg-2", "content": "",
            "author": { "user_openid": "U1" },
            "attachments": [
                { "contentType": 1, "url": "https://multimedia.qq.com/qr.jpg" },
                { "contentType": 2, "url": "https://multimedia.qq.com/other" }
            ]
        }
    });
    assert_eq!(
        parse_dispatch(&frame),
        GatewayEvent::C2cMessage {
            msg_id: "msg-2".into(),
            content: "".into(),
            user_openid: "U1".into(),
            image_url: Some("multimedia.qq.com/qr.jpg".into()),
            attachments: vec![MessageAttachment {
                kind: AttachmentKind::Image,
                url: "multimedia.qq.com/qr.jpg".into(),
                filename: None,
                size: None,
                asr_text: None,
            }],
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
fn reconnect_and_invalid_session_ops_force_reidentify() {
    // op=7（平台要求重连）与 op=9（会话失效）都会作废旧 session_id：
    // 沿用它 RESUME 会得到「连得上但不再收消息」的静默连接。
    assert!(op_forces_reidentify(7));
    assert!(op_forces_reidentify(9));
    // 正常投递与心跳 ACK 必须保留会话，否则白白丢掉可恢复的 RESUME 机会。
    assert!(!op_forces_reidentify(0));
    assert!(!op_forces_reidentify(6));
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

#[test]
fn parses_c2c_voice_message() {
    let frame = json!({
        "op": 0, "t": "C2C_MESSAGE_CREATE",
        "d": {
            "id": "msg-voice",
            "content": "",
            "author": { "user_openid": "U1" },
            "attachments": [{
                "content_type": "voice",
                "url": "multimedia.qq.com/voice.silk",
                "voice_wav_url": "multimedia.qq.com/voice.wav",
                "asr_refer_text": "明天下午有空吗"
            }]
        }
    });
    assert_eq!(
        parse_dispatch(&frame),
        GatewayEvent::C2cMessage {
            msg_id: "msg-voice".into(),
            content: "".into(),
            user_openid: "U1".into(),
            image_url: None,
            attachments: vec![MessageAttachment {
                kind: AttachmentKind::Voice,
                url: "multimedia.qq.com/voice.wav".into(),
                filename: None,
                size: None,
                asr_text: Some("明天下午有空吗".into()),
            }],
        }
    );
}

#[test]
fn parses_group_file_message() {
    let frame = json!({
        "op": 0, "t": "GROUP_AT_MESSAGE_CREATE",
        "d": {
            "id": "msg-file",
            "content": " 帮我看看这个文件 ",
            "group_openid": "G1",
            "author": { "member_openid": "U9" },
            "attachments": [{
                "content_type": "file",
                "url": "multimedia.qq.com/report.pdf",
                "filename": "report.pdf",
                "size": 20480
            }]
        }
    });
    assert_eq!(
        parse_dispatch(&frame),
        GatewayEvent::GroupMessage {
            msg_id: "msg-file".into(),
            content: "帮我看看这个文件".into(),
            group_openid: "G1".into(),
            author_openid: "U9".into(),
            attachments: vec![MessageAttachment {
                kind: AttachmentKind::File,
                url: "multimedia.qq.com/report.pdf".into(),
                filename: Some("report.pdf".into()),
                size: Some(20480),
                asr_text: None,
            }],
        }
    );
}
