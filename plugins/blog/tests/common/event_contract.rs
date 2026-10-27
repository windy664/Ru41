use ru41::{domain::AgentResult, ports::storage::TaskStore};
use serde_json::json;

pub fn assert_event_contract(store: &dyn TaskStore) {
    let task = store.enqueue("events:one", "secret-prompt").unwrap().task;
    store.enqueue("events:one", "secret-prompt").unwrap();
    assert!(store.enqueue("events:one", "conflict").is_err());
    let first = store.claim("a", 100, 10).unwrap().unwrap();
    store.heartbeat(&first, 101, 10).unwrap();
    let second = store.claim("b", 112, 30).unwrap().unwrap();
    let output = AgentResult {
        summary: "secret-result".into(),
    };
    assert!(!store.succeed(&first, &output, None, 113).unwrap());
    let run = format!("run-{}-{}", task.id, second.attempt_id);
    store
        .put_document(
            "run",
            &run,
            None,
            &json!({
                "task_id":task.id,"attempt_id":second.attempt_id,"status":"running"
            }),
        )
        .unwrap();
    let interaction = json!({"id":"approval", "run_id":run,"request_id":1,"method":"test",
        "detail":{"secret":"secret-detail"},"options":["allow"],"expires_at":140,"status":"pending","response":null});
    assert!(
        store
            .put_document("interaction", "approval", None, &interaction)
            .unwrap()
    );
    assert!(
        !store
            .put_document("interaction", "approval", None, &interaction)
            .unwrap()
    );
    assert!(
        !store
            .put_document("interaction", "approval", Some(99), &interaction)
            .unwrap()
    );
    assert!(
        !store
            .reply_interaction("approval", &json!("deny"), 113)
            .unwrap()
    );
    assert!(
        store
            .reply_interaction("approval", &json!("allow"), 113)
            .unwrap()
    );
    assert!(
        !store
            .reply_interaction("approval", &json!("allow"), 113)
            .unwrap()
    );
    assert!(store.succeed(&second, &output, None, 114).unwrap());
    let events = store.events_after(0, 100).unwrap();
    assert_eq!(
        events.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(),
        vec![
            "task.queued",
            "task.running",
            "task.retry_scheduled",
            "task.running",
            "interaction.requested",
            "interaction.answered",
            "task.succeeded"
        ]
    );
    assert!(events.windows(2).all(|w| w[0].id < w[1].id));
    assert_eq!(events[4].task_id, Some(task.id));
    assert_eq!(events[4].attempt_id, Some(second.attempt_id));
    assert_eq!(events[6].attempt_id, Some(second.attempt_id));
    assert_eq!(events[4].run_id.as_deref(), Some(run.as_str()));
    assert!(!serde_json::to_string(&events).unwrap().contains("secret-"));
    assert_eq!(store.events_after(events[2].id, 2).unwrap().len(), 2);
    let cursor = store.event_cursor().unwrap();
    assert!(store.events_after(cursor, 100).unwrap().is_empty());
    assert!(store.events_after(-1, 100).is_err());
    assert!(store.events_after(0, 0).is_err());

    let cancelled = store.enqueue("events:cancel", "x").unwrap().task;
    store.cancel(cancelled.id).unwrap();
    store.cancel(cancelled.id).unwrap();
    let failed = store.enqueue("events:fail", "x").unwrap().task;
    let claim = store.claim("c", 200, 30).unwrap().unwrap();
    assert_eq!(claim.task.id, failed.id);
    assert!(
        store
            .fail(&claim, "secret-error", 201, 10, false, false)
            .unwrap()
    );
    store.retry(failed.id).unwrap();
    let expired = json!({"status":"pending","run_id":run});
    store
        .put_document("interaction", "expires", None, &expired)
        .unwrap();
    store
        .put_document(
            "interaction",
            "expires",
            Some(1),
            &json!({"status":"expired","run_id":run}),
        )
        .unwrap();
    assert_eq!(
        store
            .events_after(cursor, 100)
            .unwrap()
            .iter()
            .map(|e| e.kind.as_str())
            .collect::<Vec<_>>(),
        vec![
            "task.queued",
            "task.cancelled",
            "task.queued",
            "task.running",
            "task.failed",
            "task.queued",
            "interaction.requested",
            "interaction.expired"
        ]
    );
}
