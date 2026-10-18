#![cfg(unix)]

use serde_json::Value;
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn run(root: &Path, args: &[&str]) -> Output {
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    Command::new(env!("CARGO_BIN_EXE_ru41"))
        .current_dir(root)
        .args(args)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap())
        .env("HOME", home)
        .output()
        .unwrap()
}

fn json(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "command failed: {}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn config_arg(root: &Path) -> String {
    root.join("ru41.toml").display().to_string()
}

#[test]
fn init_is_idempotent_and_operation_commands_share_state() {
    let root = tempfile::tempdir().unwrap();
    let config = config_arg(root.path());
    let first = json(&run(
        root.path(),
        &["--config", &config, "init", "--non-interactive"],
    ));
    assert_eq!(first["created"], true);
    assert!(root.path().join(".ru41/state.db").is_file());
    assert!(root.path().join(".ru41/runs").is_dir());
    assert!(root.path().join(".ru41/backups").is_dir());
    let initial = std::fs::read_to_string(root.path().join("ru41.toml")).unwrap();
    let mode = std::fs::metadata(root.path().join("ru41.toml"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o077, 0);

    let second = json(&run(
        root.path(),
        &["--config", &config, "init", "--non-interactive"],
    ));
    assert_eq!(second["created"], false);
    assert_eq!(
        std::fs::read_to_string(root.path().join("ru41.toml")).unwrap(),
        initial
    );

    let path = json(&run(root.path(), &["--config", &config, "config", "path"]));
    assert_eq!(path["exists"], true);
    let show = json(&run(root.path(), &["--config", &config, "config", "show"]));
    assert_eq!(show["config"]["store"]["kind"], "sqlite");
    let check = json(&run(root.path(), &["--config", &config, "config", "check"]));
    assert_eq!(check["ok"], true);
    let status = json(&run(root.path(), &["--config", &config, "status"]));
    assert_eq!(status["tasks"]["queued"], 0);
    assert_eq!(status["backends"].as_array().unwrap().len(), 2);
    let dump = json(&run(root.path(), &["--config", &config, "dump"]));
    assert_eq!(dump["environment"]["os"], std::env::consts::OS);
}

#[test]
fn missing_and_invalid_configs_are_reported_without_side_effects() {
    let root = tempfile::tempdir().unwrap();
    let config = config_arg(root.path());
    let path = json(&run(root.path(), &["--config", &config, "config", "path"]));
    assert_eq!(path["exists"], false);
    assert!(!root.path().join("ru41.toml").exists());

    std::fs::write(root.path().join("ru41.toml"), "not toml [").unwrap();
    let output = run(root.path(), &["--config", &config, "config", "check"]);
    assert!(!output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["ok"], false);
}

#[test]
fn sensitive_config_is_redacted_from_show_and_dump() {
    let root = tempfile::tempdir().unwrap();
    let db = root.path().join("state.db");
    std::fs::write(
        root.path().join("ru41.toml"),
        format!(
            r#"
[store]
kind = "sqlite"
path = "{}"
[agent.codex]
bin = "codex"
config_overrides = ['api_key="override-secret"']
[agent.codex.provider]
id = "mimo"
base_url = "https://user:pass@example.com/v1"
api_key_env = "MIMO_API_KEY"
wire_api = "responses"
[channel.qq]
app_id = "private-app-id"
app_secret = "private-app-secret"
default_notify_target = "user:private-openid"
[channel.feishu]
webhook_url = "https://example.com/private-webhook-token"
[sources.rss]
feeds = ["https://private.example/feed"]
interests = "private interests"
keywords = ["private keyword"]
[sources.ecard]
base_url = "https://private-school.example"
token = "private-ecard-token"
impl_type = "private-impl"
school_area_no = "private-area"
building_no = "private-building"
room_num = "private-room"
[api]
token = "private-api-token"
"#,
            db.display()
        ),
    )
    .unwrap();
    let config = config_arg(root.path());
    let show = run(root.path(), &["--config", &config, "config", "show"]);
    let dump = run(root.path(), &["--config", &config, "dump"]);
    for output in [show, dump] {
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout).unwrap();
        for secret in [
            "override-secret",
            "user:pass",
            "private-app-id",
            "private-app-secret",
            "private-openid",
            "private-webhook-token",
            "private.example",
            "private interests",
            "private keyword",
            "private-school",
            "private-ecard-token",
            "private-impl",
            "private-area",
            "private-building",
            "private-room",
            "private-api-token",
        ] {
            assert!(!text.contains(secret), "leaked {secret} in {text}");
        }
    }
}

fn fake_codex(root: &Path) -> PathBuf {
    let path = root.join("fake-codex");
    let marker = root.join("invocations.log");
    std::fs::write(
        &path,
        format!(
            r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> '{}'
if [ "${{1:-}}" = '--version' ]; then
  echo 'codex-cli fake'
  exit 0
fi
output=''
while [ "$#" -gt 0 ]; do
  if [ "$1" = '--output-last-message' ]; then shift; output="$1"; fi
  shift
done
cat > /dev/null
printf '%s\n' '{{"type":"turn.completed"}}'
printf '%s' '{{"summary":"ok"}}' > "$output"
"#,
            marker.display()
        ),
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&path, permissions).unwrap();
    path
}

fn write_fake_config(root: &Path, bin: &Path) {
    std::fs::write(
        root.join("ru41.toml"),
        format!(
            "[store]\nkind = \"sqlite\"\npath = \".ru41/state.db\"\n[worker]\nruns_dir = \".ru41/runs\"\n[agent.codex]\nbin = \"{}\"\n",
            bin.display()
        ),
    )
    .unwrap();
}

#[test]
fn doctor_is_offline_by_default_and_deep_runs_fake_backend() {
    let root = tempfile::tempdir().unwrap();
    let bin = fake_codex(root.path());
    write_fake_config(root.path(), &bin);
    let config = config_arg(root.path());
    let output = json(&run(root.path(), &["--config", &config, "doctor"]));
    assert_eq!(output["ok"], true);
    assert_eq!(output["model_invoked"], false);
    let invocations = std::fs::read_to_string(root.path().join("invocations.log")).unwrap();
    assert_eq!(invocations.lines().count(), 1);
    assert_eq!(invocations.trim(), "--version");

    json(&run(
        root.path(),
        &["--config", &config, "init", "--non-interactive"],
    ));
    let output = json(&run(
        root.path(),
        &["--config", &config, "doctor", "--deep"],
    ));
    assert_eq!(output["ok"], true);
    assert_eq!(output["model_invoked"], true);
    assert!(
        output["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|check| { check["name"] == "model_smoke" && check["status"] == "ok" })
    );
}
