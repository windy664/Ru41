//! 实网联调测试（默认跳过）。
//!
//! 真跑一遍听悟的整条链路（上传 → 转写 → 取逐字稿），用的是**插件自己的
//! 客户端代码**（不是 curl 复刻）—— curl 跑通不代表 Rust 这边跑得通。
//!
//! ```bash
//! RU41_TINGWU_COOKIE="$(cat /tmp/tw/cookie.txt)" \
//!   cargo test --test live -- --ignored --nocapture
//! ```
//!
//! ⚠️ cookie 只从**环境变量**读，不落进仓库（CI 有密钥扫描，真 token 进 git 就出事）。
//! 没给环境变量就跳过（当普通测试跑也不报错）。

use std::time::Duration;

use ru41_plugin_tingwu::client::{TingwuClient, parse_sentences, render_markdown};
use ru41_plugin_tingwu::config::TingwuSection;

/// 造一段 3 秒静音 WAV（44.1kHz 单声道 16bit）。
fn silent_wav(path: &std::path::Path) {
    let sample_rate: u32 = 44_100;
    let seconds: u32 = 3;
    let samples = (sample_rate * seconds) as usize;
    let data_len = samples * 2;
    // ⚠️ 长度字段必须是 **u32**（4 字节）：写成 usize 时 to_le_bytes() 给 8 字节，
    // RIFF 头就胖成 52 字节 → 服务端解不了 → DefaultError（2026-11-25 实测踩到）。
    let data_len32 = data_len as u32;
    let mut bytes = Vec::with_capacity(44 + data_len);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len32).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
    bytes.extend_from_slice(&1u16.to_le_bytes()); // 单声道
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // byte rate
    bytes.extend_from_slice(&2u16.to_le_bytes()); // block align
    bytes.extend_from_slice(&16u16.to_le_bytes()); // bits
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len32.to_le_bytes());
    bytes.resize(bytes.len() + data_len, 0);
    std::fs::write(path, bytes).expect("写 wav 失败");
}

#[tokio::test]
#[ignore = "要真网与真 cookie"]
async fn transcribes_a_real_file_end_to_end() {
    let cookie = std::env::var("RU41_TINGWU_COOKIE").expect("要 RU41_TINGWU_COOKIE");
    let dir = tempfile::tempdir().expect("临时目录");
    let path = dir.path().join("silence.wav");
    silent_wav(&path);

    let section = TingwuSection {
        poll_timeout_secs: Some(300),
        poll_interval_secs: Some(5),
        ..Default::default()
    };
    let client = TingwuClient::new(&cookie, &section).expect("建客户端");

    let transcript = client.transcribe(&path).await.expect("转写");

    // 静音文件没有内容是正常的：链路走通 = status 0 + 结果结构可解析。
    assert!(!transcript.trans_id.is_empty(), "任务 ID 不该为空");
    let sentences = parse_sentences(&transcript.raw);
    assert_eq!(
        sentences.len(),
        transcript.sentences.len(),
        "raw 与解析出的句子数对不上"
    );
    println!(
        "trans_id={} duration={} wordCount={} 段落={}",
        transcript.trans_id,
        transcript.duration_secs,
        transcript.word_count,
        transcript.segments.len()
    );
}

#[tokio::test]
#[ignore = "要真网与真 cookie"]
async fn lists_real_tasks() {
    let cookie = std::env::var("RU41_TINGWU_COOKIE").expect("要 RU41_TINGWU_COOKIE");
    let section = TingwuSection::default();
    let client = TingwuClient::new(&cookie, &section).expect("建客户端");
    let tasks = client.list(5).await.expect("列任务");
    assert!(!tasks.is_empty(), "账号里至少有刚转的那个任务");
    for task in &tasks {
        assert!(!task.trans_id.is_empty());
    }
    println!("最近任务: {:?}", tasks.first().map(|t| &t.trans_id));
    tokio::time::sleep(Duration::from_millis(1)).await;
}

/// 真录音走一遍：验证说话人分离与时间戳解析（用有内容的文件，不是静音）。
///
/// `RU41_TINGWU_FILE=/path/to/real.mp3`
#[tokio::test]
#[ignore = "要真网与真 cookie 与真录音文件"]
async fn transcribes_a_real_recording_with_speakers() {
    let cookie = std::env::var("RU41_TINGWU_COOKIE").expect("要 RU41_TINGWU_COOKIE");
    let file = std::env::var("RU41_TINGWU_FILE").expect("要 RU41_TINGWU_FILE");
    let path = std::path::PathBuf::from(file);

    let section = TingwuSection {
        poll_timeout_secs: Some(600),
        poll_interval_secs: Some(5),
        ..Default::default()
    };
    let client = TingwuClient::new(&cookie, &section).expect("建客户端");
    let transcript = client.transcribe(&path).await.expect("转写");

    assert!(transcript.word_count > 0, "真录音应该有字数");
    assert!(!transcript.sentences.is_empty(), "应该有句子");
    // 说话人分离：si 从 1 起（网页版显示的「说话人 1」）。
    assert!(
        transcript.sentences.iter().all(|s| s.speaker >= 1),
        "说话人编号不该从 0 起"
    );
    // 时间戳：句子按时间递增。
    let mut previous = i64::MIN;
    for sentence in &transcript.sentences {
        assert!(sentence.begin_ms >= previous, "句子时间戳应递增");
        assert!(sentence.end_ms >= sentence.begin_ms, "结束时间不该早于开始");
        previous = sentence.begin_ms;
    }
    // 合并后的段落数不该比句子数多。
    assert!(transcript.segments.len() <= transcript.sentences.len());
    println!(
        "trans_id={} 时长={}s 字数={} 句={} 段={} 说话人={}",
        transcript.trans_id,
        transcript.duration_secs,
        transcript.word_count,
        transcript.sentences.len(),
        transcript.segments.len(),
        transcript.speaker_count()
    );
    let markdown = render_markdown(&transcript);
    let head: String = markdown.chars().take(600).collect();
    println!("--- markdown 前 600 字 ---\n{head}");
}
