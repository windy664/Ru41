use ru41::{
    adapters::database::sqlite::SqliteTaskStore, domain::records::Interaction,
    ports::storage::TaskStore,
};
use serde_json::json;
use std::sync::{Arc, Barrier};
fn seed(store: &dyn TaskStore) -> ru41::domain::Claim {
    let task = store.enqueue("approval-test", "analysis").unwrap().task;
    let claim = store.claim("worker", 100, 10).unwrap().unwrap();
    store
        .put_document(
            "run",
            "run-test",
            None,
            &json!({"task_id":task.id,"attempt_id":claim.attempt_id,"status":"running"}),
        )
        .unwrap();
    store
        .put_document(
            "interaction",
            "interaction-test",
            None,
            &serde_json::to_value(Interaction {
                id: "interaction-test".into(),
                run_id: "run-test".into(),
                request_id: json!(1),
                method: "approval".into(),
                detail: json!({}),
                options: vec![json!("yes"), json!("no")],
                expires_at: 200,
                status: "pending".into(),
                response: None,
            })
            .unwrap(),
        )
        .unwrap();
    claim
}
#[test]
fn rejects_old_attempt_after_reclaim_without_consuming_request() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteTaskStore::open(&dir.path().join("db")).unwrap();
    let old = seed(&store);
    let new = store.claim("new-worker", 111, 20).unwrap().unwrap();
    assert_ne!(old.attempt_id, new.attempt_id);
    assert!(
        !store
            .reply_interaction("interaction-test", &json!("yes"), 112)
            .unwrap()
    );
    assert_eq!(
        store
            .document("interaction", "interaction-test")
            .unwrap()
            .unwrap()
            .value["status"],
        "pending"
    );
}
#[test]
fn rejects_expired_lease_cancelled_task_and_invalid_option() {
    for mode in ["lease", "cancel", "option"] {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteTaskStore::open(&dir.path().join("db")).unwrap();
        let claim = seed(&store);
        if mode == "cancel" {
            store.cancel(claim.task.id).unwrap();
        }
        let response = if mode == "option" {
            json!("other")
        } else {
            json!("yes")
        };
        assert!(
            !store
                .reply_interaction(
                    "interaction-test",
                    &response,
                    if mode == "lease" { 110 } else { 101 }
                )
                .unwrap()
        );
    }
}
#[test]
fn concurrent_connections_allow_only_one_reply() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let store = SqliteTaskStore::open(&path).unwrap();
    seed(&store);
    let barrier = Arc::new(Barrier::new(2));
    let joins: Vec<_> = (0..2)
        .map(|_| {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let store = SqliteTaskStore::open(&path).unwrap();
                barrier.wait();
                store
                    .reply_interaction("interaction-test", &json!("yes"), 101)
                    .unwrap()
            })
        })
        .collect();
    assert_eq!(
        joins
            .into_iter()
            .map(|j| usize::from(j.join().unwrap()))
            .sum::<usize>(),
        1
    );
}
