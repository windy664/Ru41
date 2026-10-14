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
        executable: bin,
        model: None,
    };
    let output = agent.run(request(dir.path())).await.unwrap();
    assert_eq!(output.session_id.as_deref(), Some("abc"));
    assert_eq!(output.result.summary, "hello");
    assert!(dir.path().join("run/events.jsonl").is_file());
    assert!(
        std::fs::read_to_string(dir.path().join("run/input.txt"))
            .unwrap()
            .contains("Request:\nhello")
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
async fn drains_stderr_concurrently_and_limits_log_size() {
    let dir = tempfile::tempdir().unwrap();
    let agent = CodexAgent {
        config_overrides: Vec::new(),
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
