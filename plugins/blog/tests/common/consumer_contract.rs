use ru41::{
    domain::consumers::{ConsumerSpec, DeliveryFailure},
    ports::storage::TaskStore,
};

pub fn assert_consumer_contract(store: &dyn TaskStore) {
    for n in 0..3 {
        store.enqueue(&format!("consumer:{n}"), "private").unwrap();
    }
    let spec = ConsumerSpec {
        name: "test.a".into(),
        start_after: 0,
        max_attempts: 2,
    };
    store.register_consumer(&spec).unwrap();
    store
        .register_consumer(&ConsumerSpec {
            name: "test.b".into(),
            ..spec.clone()
        })
        .unwrap();
    let old = store.claim_event("test.a", 100, 5).unwrap().unwrap();
    assert!(store.claim_event("test.a", 100, 5).unwrap().is_none());
    let other = store.claim_event("test.b", 100, 5).unwrap().unwrap();
    assert_eq!(old.event.id, other.event.id);
    assert_ne!(old.idempotency_key(), other.idempotency_key());
    assert!(store.renew_event(&old, 101, 5).unwrap());
    assert!(store.claim_event("test.a", 105, 5).unwrap().is_none());
    let current = store.claim_event("test.a", 106, 5).unwrap().unwrap();
    assert_eq!(old.idempotency_key(), current.idempotency_key());
    assert!(current.generation > old.generation);
    assert!(!store.complete_event(&old, 106).unwrap());
    assert!(!store.renew_event(&old, 106, 5).unwrap());
    assert!(
        !store
            .fail_event(&old, 106, 107, DeliveryFailure::Handler)
            .unwrap()
    );
    assert!(
        store
            .fail_event(&current, 107, 109, DeliveryFailure::Handler)
            .unwrap()
    );
    let state = store.consumer("test.a").unwrap().unwrap();
    assert!(state.blocked);
    assert_eq!(state.cursor, 0);
    assert_eq!(state.active_event_id, Some(old.event.id));
    assert!(store.claim_event("test.a", 110, 5).unwrap().is_none());
    assert!(store.retry_consumer("test.a", 110).unwrap());
    assert!(!store.retry_consumer("test.a", 110).unwrap());
    let recovered = store.claim_event("test.a", 110, 5).unwrap().unwrap();
    assert_eq!(recovered.attempt, 1);
    assert_eq!(recovered.idempotency_key(), old.idempotency_key());
    assert!(store.complete_event(&recovered, 111).unwrap());
    assert!(!store.complete_event(&recovered, 111).unwrap());
    let second = store.claim_event("test.a", 112, 5).unwrap().unwrap();
    assert!(second.event.id > old.event.id);
    assert!(
        store
            .fail_event(&second, 113, 120, DeliveryFailure::Timeout)
            .unwrap()
    );
    assert!(store.claim_event("test.a", 119, 5).unwrap().is_none());
    let second_retry = store.claim_event("test.a", 120, 5).unwrap().unwrap();
    assert_eq!(second_retry.event.id, second.event.id);
    assert!(store.complete_event(&second_retry, 121).unwrap());
    assert_eq!(
        store.register_consumer(&spec).unwrap().cursor,
        second.event.id
    );
    assert!(
        store
            .register_consumer(&ConsumerSpec {
                max_attempts: 3,
                ..spec.clone()
            })
            .is_err()
    );
    assert!(
        store
            .register_consumer(&ConsumerSpec {
                name: "future".into(),
                start_after: 99999,
                ..spec.clone()
            })
            .is_err()
    );
    assert!(
        store
            .register_consumer(&ConsumerSpec {
                name: "bad name".into(),
                ..spec.clone()
            })
            .is_err()
    );
    // 最后一轮崩溃也必须阻塞，不能无限认领或跳到下一事件。
    store
        .register_consumer(&ConsumerSpec {
            name: "crash".into(),
            max_attempts: 1,
            ..spec.clone()
        })
        .unwrap();
    let crash = store.claim_event("crash", 200, 5).unwrap().unwrap();
    assert!(!store.complete_event(&crash, 205).unwrap());
    assert!(store.claim_event("crash", 205, 5).unwrap().is_none());
    let state = store.consumer("crash").unwrap().unwrap();
    assert!(state.blocked);
    assert_eq!(state.last_failure.as_deref(), Some("lease_expired"));
    assert_eq!(state.cursor, 0);
    assert!(store.consumer("missing").unwrap().is_none());
    assert!(store.claim_event("missing", 100, 5).is_err());
}
