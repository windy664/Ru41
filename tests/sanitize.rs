use regex::Regex;
use ru41::{
    adapters::database::sqlite::SqliteTaskStore,
    app::{
        agent_service::{AgentBackend, AgentOutput, AgentRequest, BackendDescriptor},
        sanitize::{Mapping, NoopSanitizer, RandomSanitizer, Sanitizer},
        store::TaskStore,
        worker::{Worker, WorkerConfig},
    },
    domain::{AgentResult, RunContract},
};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::watch;

fn sanitizer() -> RandomSanitizer {
    RandomSanitizer::default()
}

#[test]
fn phone_is_masked_with_valid_fake() {
    let text = "我的手机号是 13812345678，回电联系";
    let (masked, mapping) = sanitizer().sanitize(text);
    assert!(!masked.contains("13812345678"));
    assert_eq!(mapping.entries.len(), 1);
    let fake = &mapping.entries[0].fake;
    assert!(
        Regex::new(r"^1[3-9]\d{9}$").unwrap().is_match(fake),
        "invalid fake phone: {fake}"
    );
    assert_eq!(sanitizer().restore(&masked, &mapping), text);
}

#[test]
fn id_card_is_masked_with_checksum_valid_fake() {
    let text = "身份证号 11010119900307432X 请保密";
    let (masked, mapping) = sanitizer().sanitize(text);
    assert!(!masked.contains("11010119900307432X"));
    let fake = &mapping.entries[0].fake;
    const WEIGHTS: &[u32] = &[7, 9, 10, 5, 8, 4, 2, 1, 6, 3, 7, 9, 10, 5, 8, 4, 2];
    const CHECKS: &[char] = &['1', '0', 'X', '9', '8', '7', '6', '5', '4', '3', '2'];
    let sum: u32 = fake[..17]
        .bytes()
        .zip(WEIGHTS)
        .map(|(d, w)| u32::from(d - b'0') * w)
        .sum();
    assert_eq!(fake.chars().nth(17).unwrap(), CHECKS[(sum % 11) as usize]);
    assert_eq!(sanitizer().restore(&masked, &mapping), text);
}

#[test]
fn email_is_masked_with_valid_fake() {
    let text = "邮箱 zhang.san@corp-example.com 备用";
    let (masked, mapping) = sanitizer().sanitize(text);
    assert!(!masked.contains("zhang.san@corp-example.com"));
    let fake = &mapping.entries[0].fake;
    assert!(
        Regex::new(r"^[\w.+-]+@[\w-]+(\.[\w-]+)+$")
            .unwrap()
            .is_match(fake)
    );
    assert_eq!(sanitizer().restore(&masked, &mapping), text);
}

#[test]
fn bank_card_is_masked_with_luhn_valid_fake() {
    let text = "卡号 6217003810026584320 转账";
    let (masked, mapping) = sanitizer().sanitize(text);
    assert!(!masked.contains("6217003810026584320"));
    let fake = &mapping.entries[0].fake;
    let sum: u32 = fake
        .bytes()
        .rev()
        .enumerate()
        .map(|(i, d)| {
            let mut v = u32::from(d - b'0');
            if i % 2 == 1 {
                v *= 2;
                if v > 9 {
                    v -= 9;
                }
            }
            v
        })
        .sum();
    assert_eq!(sum % 10, 0, "Luhn check failed for {fake}");
    assert_eq!(sanitizer().restore(&masked, &mapping), text);
}

#[test]
fn name_with_prefix_is_masked() {
    let text = "姓名：张伟明 负责对接";
    let (masked, mapping) = sanitizer().sanitize(text);
    assert!(!masked.contains("张伟明"));
    assert!(masked.contains("姓名："));
    assert_eq!(sanitizer().restore(&masked, &mapping), text);
}

#[test]
fn same_real_value_maps_to_same_fake() {
    let text = "先打 13812345678，再确认 13812345678 是否本人";
    let (masked, mapping) = sanitizer().sanitize(text);
    assert_eq!(mapping.entries.len(), 1);
    assert!(!masked.contains("13812345678"));
    let fake = &mapping.entries[0].fake;
    assert_eq!(masked.matches(fake.as_str()).count(), 2);
}

#[test]
fn fake_values_never_collide_with_original_text() {
    // 原文已含若干合法手机号，假值不得撞上其中任何一个。
    let text = "13812345678 13900000000 13711112222 13812345678".repeat(20);
    let (_, mapping) = sanitizer().sanitize(&text);
    for entry in &mapping.entries {
        let others: Vec<&str> = mapping
            .entries
            .iter()
            .filter(|e| e.real != entry.real)
            .map(|e| e.real.as_str())
            .collect();
        assert!(!others.contains(&entry.fake.as_str()));
    }
}

#[test]
fn text_without_sensitive_data_is_unchanged() {
    let text = "整理今天的收件箱，把论文笔记归档到 03-Knowledge";
    let (masked, mapping) = sanitizer().sanitize(text);
    assert_eq!(masked, text);
    assert!(mapping.is_empty());
}

#[test]
fn noop_sanitizer_passes_through() {
    let text = "手机号 13812345678";
    let (masked, mapping) = NoopSanitizer.sanitize(text);
    assert_eq!(masked, text);
    assert!(mapping.is_empty());
    assert_eq!(NoopSanitizer.restore("x", &Mapping::default()), "x");
}

struct CapturingAgent {
    seen: Arc<Mutex<Option<String>>>,
}
impl AgentBackend for CapturingAgent {
    fn descriptor(&self) -> BackendDescriptor {
        BackendDescriptor {
            id: "codex".into(),
            model: None,
            contracts: vec![RunContract::AnalysisV1],
            supports_resume: false,
        }
    }
    fn run(
        &self,
        request: AgentRequest,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<AgentOutput>> + Send + '_>> {
        *self.seen.lock().unwrap() = Some(request.prompt);
        Box::pin(async move {
            Ok(AgentOutput {
                // Agent 只会引用脱敏后的假值。
                result: AgentResult {
                    summary: format!("已回电 {}", self.seen.lock().unwrap().clone().unwrap()),
                },
                session_id: None,
            })
        })
    }
}

#[tokio::test]
async fn agent_sees_masked_prompt_and_store_keeps_restored_result() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let seen = Arc::new(Mutex::new(None));
    let mut backends = ru41::app::agent_service::BackendRegistry::default();
    backends
        .register(CapturingAgent { seen: seen.clone() })
        .unwrap();
    let mut worker = Worker {
        store: store.clone() as Arc<dyn TaskStore>,
        backends: Arc::new(backends),
        sanitizer: Arc::new(RandomSanitizer::default()),
        config: WorkerConfig {
            worker_id: "test".into(),
            runs_dir: dir.path().join("runs"),
            timeout: Duration::from_secs(2),
            lease_secs: 10,
            retry_delay_secs: 10,
        },
    };
    let task = store
        .enqueue("event", "帮我回电 13812345678 确认行程")
        .unwrap()
        .task;
    let (_sender, receiver) = watch::channel(false);
    worker.run_once(receiver).await.unwrap().unwrap();
    let prompt = seen.lock().unwrap().clone().unwrap();
    assert!(!prompt.contains("13812345678"));
    assert!(prompt.contains("帮我回电"));
    let result = store.get(task.id).unwrap().unwrap().result.unwrap();
    assert!(result.summary.contains("13812345678"));
    // 审计：映射表随尝试目录落盘，且只含假值与真值的对应关系。
    let attempts = store.attempts(task.id).unwrap();
    let mapping_file =
        std::path::Path::new(attempts[0].run_dir.as_deref().unwrap()).join("mapping.json");
    let mapping: Mapping =
        serde_json::from_str(&std::fs::read_to_string(mapping_file).unwrap()).unwrap();
    assert_eq!(mapping.entries.len(), 1);
    assert_eq!(mapping.entries[0].real, "13812345678");
}
