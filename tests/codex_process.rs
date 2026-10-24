#![cfg(unix)]
use ru41::{
    adapters::agent::codex::CodexAgent,
    ports::agent::{AgentBackend, AgentRequest},
};
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

fn executable(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("fake-codex");
    let script = format!(
        r#"#!/bin/sh
set -eu
printf '%s\n' "$@" > received-args.txt
output=''
while [ "$#" -gt 0 ]; do
  if [ "$1" = '--output-last-message' ]; then shift; output="$1"; fi
  shift
done
cat > input.txt
{body}
"#
    );
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path
}
fn request(dir: &Path) -> AgentRequest {
    let run = dir.join("run");
    std::fs::create_dir_all(&run).unwrap();
    AgentRequest {
        context: None,
        prompt: "hello".into(),
        run_dir: run,
        contract: ru41::domain::RunContract::AnalysisV1,
    }
}

#[tokio::test]
async fn parses_events_and_validated_final_json() {
    let dir = tempfile::tempdir().unwrap();
    let bin = executable(
        dir.path(),
        r#"
printf '%s\n' '{"type":"thread.started","thread_id":"abc"}' '{"type":"turn.completed"}'
printf '%s' '{"summary":"hello"}' > "$output"
echo 'diagnostic' >&2
"#,
    );
    let agent = CodexAgent {
        config_overrides: Vec::new(),
        output_schema: true,
        executable: bin,
        model: None,
    };
    let output = agent.run(request(dir.path())).await.unwrap();
    assert_eq!(output.session_id.as_deref(), Some("abc"));
    assert_eq!(output.result.summary, "hello");
    assert!(dir.path().join("run/events.jsonl").is_file());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("run/input.txt")).unwrap(),
        "hello"
    );
}

#[tokio::test]
async fn rejects_nonzero_exit_bad_json_and_missing_completion() {
    let cases = [
        "echo failed >&2; exit 7",
        "echo '{\"type\":\"turn.completed\"}'; echo 'not JSON' > \"$output\"",
        "echo '{\"type\":\"turn.completed\"}'; echo '{\"summary\":\" \"}' > \"$output\"",
        "echo '{\"type\":\"turn.failed\"}'; echo '{\"summary\":\"hello\"}' > \"$output\"",
        "echo '{\"summary\":\"hello\"}' > \"$output\"",
        "echo '{\"type\":\"turn.completed\"}'; echo '{\"summary\":\"ok\",\"unexpected\":true}' > \"$output\"",
    ];
    for body in cases {
        let dir = tempfile::tempdir().unwrap();
        let agent = CodexAgent {
            config_overrides: Vec::new(),
            output_schema: true,
            executable: executable(dir.path(), body),
            model: None,
        };
        assert!(
            agent.run(request(dir.path())).await.is_err(),
            "accepted invalid case: {body}"
        );
    }
}

#[tokio::test]
async fn cli_caps_probe_controls_ephemeral_and_ignore_user_config() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("args.log");
    let make_fake = |name: &str, help_text: &str| {
        let path = dir.path().join(name);
        let script = format!(
            r#"#!/bin/sh
if [ "$1" = exec ] && [ "$2" = --help ]; then
  printf '%s\n' '{help_text}'
  exit 0
fi
printf '%s\n' "$@" > "{log}"
output=''
while [ "$#" -gt 0 ]; do
  if [ "$1" = '--output-last-message' ]; then shift; output="$1"; fi
  shift
done
cat > /dev/null
echo '{{"type":"turn.completed"}}'
printf '%s' '{{"summary":"ok"}}' > "$output"
"#,
            log = log.display()
        );
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        path
    };
    // 新版 help 声明全部能力；旧版（如 0.80，用于 GLM chat 端点）没有后两个 flag。
    let modern = make_fake("modern-codex", "--ephemeral --ignore-user-config --json");
    let legacy = make_fake("legacy-codex", "--json");

    for (bin, expect_modern) in [(modern, true), (legacy, false)] {
        std::fs::remove_file(&log).ok();
        let agent = CodexAgent {
            executable: bin.clone(),
            model: None,
            config_overrides: Vec::new(),
            output_schema: true,
        };
        agent.run(request(dir.path())).await.unwrap();
        let args = std::fs::read_to_string(&log).unwrap();
        assert_eq!(
            args.contains("--ephemeral"),
            expect_modern,
            "bin={bin:?} args={args}"
        );
        assert_eq!(
            args.contains("--ignore-user-config"),
            expect_modern,
            "bin={bin:?} args={args}"
        );
    }
}

#[tokio::test]
async fn turn_failed_surfaces_provider_error_message() {
    let dir = tempfile::tempdir().unwrap();
    let body = r#"echo '{"type":"error","message":"Reconnecting... 5/5 (unexpected status 402 Payment Required: Insufficient account balance)"}'
echo '{"type":"turn.failed","error":{"message":"unexpected status 402 Payment Required: Insufficient account balance"}}'
exit 1"#;
    let agent = CodexAgent {
        config_overrides: Vec::new(),
        output_schema: true,
        executable: executable(dir.path(), body),
        model: None,
    };
    let error = match agent.run(request(dir.path())).await {
        Ok(_) => panic!("expected failure for turn.failed case"),
        Err(error) => error,
    };
    let text = format!("{error:#}");
    assert!(text.contains("402"), "error: {text}");
    assert!(
        text.contains("Insufficient account balance"),
        "error: {text}"
    );
}

#[tokio::test]
async fn drains_stderr_concurrently_and_limits_log_size() {
    let dir = tempfile::tempdir().unwrap();
    let agent = CodexAgent {
        config_overrides: Vec::new(),
        output_schema: true,
        executable: executable(
            dir.path(),
            r#"
head -c 2200000 /dev/zero >&2
sleep 60
"#,
        ),
        model: None,
    };
    let result = tokio::time::timeout(Duration::from_secs(5), agent.run(request(dir.path()))).await;
    assert!(result.expect("log capture deadlocked").is_err());
    assert!(
        std::fs::metadata(dir.path().join("run/stderr.log"))
            .unwrap()
            .len()
            <= 2 * 1024 * 1024
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn dropping_run_terminates_subprocess_group() {
    let dir = tempfile::tempdir().unwrap();
    let agent = CodexAgent {
        config_overrides: Vec::new(),
        output_schema: true,
        executable: executable(dir.path(), "sleep 60 &\necho $! > child.pid\nwait"),
        model: None,
    };
    let req = request(dir.path());
    let pid_path = req.run_dir.join("child.pid");
    let mut execution = agent.run(req);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            tokio::select! {
                result = &mut execution => panic!("unexpected completion: {:?}", result.err()),
                _ = tokio::time::sleep(Duration::from_millis(10)) => { if pid_path.exists() { break; } }
            }
        }
    }).await.unwrap();
    let pid = std::fs::read_to_string(pid_path).unwrap();
    drop(execution);
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match std::fs::read_to_string(format!("/proc/{}/stat", pid.trim())) {
                Err(_) => break,
                Ok(stat) if stat.split_whitespace().nth(2) == Some("Z") => break,
                _ => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        }
    })
    .await
    .expect("descendant still running");
}

#[tokio::test]
async fn config_overrides_and_model_precede_stdin_positional() {
    // 假脚本回显收到的完整参数列表，断言 `-c` 覆盖项位于 stdin 位置参数之前。
    let dir = tempfile::tempdir().unwrap();
    let bin = executable(
        dir.path(),
        r#"
printf '%s\n' "$@" > args.txt
printf '%s\n' '{"type":"turn.completed"}'
printf '%s' '{"summary":"ok"}' > "$output"
"#,
    );
    let agent = CodexAgent {
        config_overrides: vec![
            "model_providers.x={name=\"x\",base_url=\"https://a.b\",wire_api=\"chat\"}".into(),
            "model_provider=\"x\"".into(),
        ],
        executable: bin,
        model: Some("gpt-test".into()),
        output_schema: true,
    };
    agent.run(request(dir.path())).await.unwrap();
    let args = std::fs::read_to_string(dir.path().join("run/received-args.txt")).unwrap();
    let argv: Vec<&str> = args.lines().collect();
    let stdin_pos = argv
        .iter()
        .position(|a| *a == "-")
        .expect("stdin marker present");
    let model_pos = argv
        .iter()
        .position(|a| *a == "--model")
        .expect("--model present");
    let provider_pos = argv
        .iter()
        .position(|a| a.starts_with("model_provider="))
        .expect("provider override present");
    assert!(
        model_pos < stdin_pos,
        "model flag must precede stdin marker: {argv:?}"
    );
    assert!(
        provider_pos < stdin_pos,
        "provider overrides must precede stdin marker: {argv:?}"
    );
}

#[tokio::test]
async fn accepts_recoverable_error_when_turn_completes() {
    let dir = tempfile::tempdir().unwrap();
    let bin = executable(
        dir.path(),
        r#"
printf '%s\n' '{"type":"error","message":"Reconnecting..."}' '{"type":"turn.completed"}'
printf '%s' '{"summary":"recovered"}' > "$output"
"#,
    );
    let agent = CodexAgent {
        config_overrides: Vec::new(),
        output_schema: true,
        executable: bin,
        model: None,
    };
    let output = agent.run(request(dir.path())).await.unwrap();
    assert_eq!(output.result.summary, "recovered");
}

#[tokio::test]
async fn native_output_schema_can_be_disabled_for_plain_text() {
    let dir = tempfile::tempdir().unwrap();
    let bin = executable(
        dir.path(),
        r#"
printf '%s\n' '{"type":"turn.completed"}'
printf '%s' 'plain text response' > "$output"
"#,
    );
    let agent = CodexAgent {
        config_overrides: Vec::new(),
        output_schema: false,
        executable: bin,
        model: None,
    };
    let output = agent.run(request(dir.path())).await.unwrap();
    assert_eq!(output.result.summary, "plain text response");
    let args = std::fs::read_to_string(dir.path().join("run/received-args.txt")).unwrap();
    assert!(!args.contains("--output-schema"));
    assert!(args.contains("--output-last-message"));
}

#[tokio::test]
async fn unexecuted_tool_call_is_transient_failure() {
    let dir = tempfile::tempdir().unwrap();
    let bin = executable(
        dir.path(),
        r#"
printf '%s\n' '{"type":"turn.completed"}'
printf '%s' '先说明一句<tool_call_x><function=exec_command>noop</function></tool_call_x>' > "$output"
"#,
    );
    let agent = CodexAgent {
        config_overrides: Vec::new(),
        output_schema: false,
        executable: bin,
        model: None,
    };
    let error = agent
        .run(request(dir.path()))
        .await
        .err()
        .expect("raw tool call must be rejected");
    assert_eq!(
        ru41::domain::error::classify(&error),
        ru41::domain::error::FailureKind::Transient
    );
    assert!(error.to_string().contains("unexecuted tool call"));
}

struct NoInteractions;

impl ru41::ports::interaction::InteractionHandler for NoInteractions {
    fn request(
        &self,
        _request: ru41::domain::records::Interaction,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = anyhow::Result<serde_json::Value>> + Send + '_>,
    > {
        Box::pin(async { anyhow::bail!("unexpected interaction") })
    }
}

#[tokio::test]
async fn resumes_native_codex_session_for_conversation() {
    let dir = tempfile::tempdir().unwrap();
    // 假 codex 不在 help 里声明 --output-last-message（旧版，如 0.80）：
    // resume 不得传该 flag，最终消息改从事件流的 agent_message 提取。
    let bin = executable(
        dir.path(),
        r#"
printf '%s\n' '{"type":"thread.started","thread_id":"thread-abc"}' '{"type":"item.completed","item":{"type":"agent_message","text":"resumed"}}' '{"type":"turn.completed"}'
"#,
    );
    let agent = CodexAgent {
        config_overrides: Vec::new(),
        output_schema: false,
        executable: bin,
        model: None,
    };
    let mut request = request(dir.path());
    request.context = Some(ru41::ports::agent::RunContext {
        run_id: "run-1".into(),
        interactions: std::sync::Arc::new(NoInteractions),
        conversation_id: Some("qq:user:U1".into()),
        resume_session_id: Some("thread-abc".into()),
        skills: vec![ru41::skills::SkillPackage {
            name: "test-skill".into(),
            description: "Test skill".into(),
            instructions: "Follow the test workflow".into(),
            files: vec![ru41::skills::SkillFile {
                relative: PathBuf::from("SKILL.md"),
                bytes: b"test".to_vec(),
            }],
        }],
    });
    let output = agent.run(request).await.unwrap();
    assert!(
        dir.path()
            .join("run/.agents/skills/test-skill/SKILL.md")
            .is_file()
    );
    assert_eq!(output.result.summary, "resumed");
    assert_eq!(output.session_id.as_deref(), Some("thread-abc"));
    let args = std::fs::read_to_string(dir.path().join("run/received-args.txt")).unwrap();
    let argv: Vec<&str> = args.lines().collect();
    assert_eq!(&argv[..3], &["exec", "resume", "thread-abc"]);
    assert!(!argv.contains(&"--output-last-message"));
    assert!(!argv.contains(&"--ephemeral"));
    assert!(!argv.contains(&"--color"));
}

#[tokio::test]
async fn modern_resume_keeps_output_last_message_flag() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("modern-codex");
    std::fs::write(
        &bin,
        r#"#!/bin/sh
if [ "$1" = exec ] && [ "$2" = --help ]; then
  printf '%s\n' '--ephemeral' '--ignore-user-config' '--json'
  exit 0
fi
if [ "$1" = exec ] && [ "$2" = resume ] && [ "$3" = --help ]; then
  printf '%s\n' '--json' '--output-last-message'
  exit 0
fi
printf '%s\n' "$@" > received-args.txt
output=''
while [ "$#" -gt 0 ]; do
  if [ "$1" = '--output-last-message' ]; then shift; output="$1"; fi
  shift
done
cat > input.txt
printf '%s\n' '{"type":"thread.started","thread_id":"thread-abc"}' '{"type":"turn.completed"}'
printf '%s' 'resumed-from-file' > "$output"
"#,
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
    let agent = CodexAgent {
        config_overrides: Vec::new(),
        output_schema: false,
        executable: bin,
        model: None,
    };
    let mut request = request(dir.path());
    request.context = Some(ru41::ports::agent::RunContext {
        run_id: "run-1".into(),
        interactions: std::sync::Arc::new(NoInteractions),
        conversation_id: Some("qq:user:U1".into()),
        resume_session_id: Some("thread-abc".into()),
        skills: vec![],
    });
    let output = agent.run(request).await.unwrap();
    assert_eq!(output.result.summary, "resumed-from-file");
    let args = std::fs::read_to_string(dir.path().join("run/received-args.txt")).unwrap();
    assert!(args.contains("--output-last-message"));
}
