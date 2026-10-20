use ru41::adapters::database::sqlite::SqliteTaskStore;
use ru41::app::identity::{BIND_MAX_ATTEMPTS, BIND_WINDOW_SECS, BindOutcome, IdentityService};
use ru41::ports::storage::TaskStore;
use std::sync::Arc;

struct Fixture {
    _dir: tempfile::TempDir,
    identity: IdentityService,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    Fixture {
        _dir: dir,
        identity: IdentityService::new(store),
    }
}

#[test]
fn binds_with_matching_secret() {
    let fixture = fixture();
    let identity = &fixture.identity;
    assert_eq!(
        identity
            .bind("qq", "U1", "s3cret", "s3cret", 1_000)
            .unwrap(),
        BindOutcome::Bound
    );
    assert!(identity.is_bound("qq", "U1").unwrap());
}

#[test]
fn locks_after_repeated_failures_and_ignores_correct_secret() {
    let fixture = fixture();
    let identity = &fixture.identity;
    for _ in 0..BIND_MAX_ATTEMPTS {
        assert_eq!(
            identity.bind("qq", "U1", "s3cret", "wrong", 1_000).unwrap(),
            BindOutcome::Rejected
        );
    }
    // 锁定后即使口令正确也拒绝，正确性不再可探测。
    assert_eq!(
        identity
            .bind("qq", "U1", "s3cret", "s3cret", 1_000)
            .unwrap(),
        BindOutcome::Locked
    );
    assert!(!identity.is_bound("qq", "U1").unwrap());
}

#[test]
fn lockout_resets_after_window() {
    let fixture = fixture();
    let identity = &fixture.identity;
    for _ in 0..BIND_MAX_ATTEMPTS {
        identity.bind("qq", "U1", "s3cret", "wrong", 1_000).unwrap();
    }
    let later = 1_000 + BIND_WINDOW_SECS;
    assert_eq!(
        identity
            .bind("qq", "U1", "s3cret", "s3cret", later)
            .unwrap(),
        BindOutcome::Bound
    );
    assert!(identity.is_bound("qq", "U1").unwrap());
}

#[test]
fn success_clears_attempt_counter() {
    let fixture = fixture();
    let identity = &fixture.identity;
    for _ in 0..(BIND_MAX_ATTEMPTS - 1) {
        identity.bind("qq", "U1", "s3cret", "wrong", 1_000).unwrap();
    }
    identity
        .bind("qq", "U1", "s3cret", "s3cret", 1_000)
        .unwrap();
    // 计数已清零，重新计算窗口。
    for _ in 0..(BIND_MAX_ATTEMPTS - 1) {
        identity.bind("qq", "U1", "s3cret", "wrong", 1_100).unwrap();
    }
    assert_eq!(
        identity
            .bind("qq", "U1", "s3cret", "s3cret", 1_100)
            .unwrap(),
        BindOutcome::Bound
    );
}

#[test]
fn attempts_are_per_user() {
    let fixture = fixture();
    let identity = &fixture.identity;
    for _ in 0..BIND_MAX_ATTEMPTS {
        identity.bind("qq", "U1", "s3cret", "wrong", 1_000).unwrap();
    }
    assert_eq!(
        identity
            .bind("qq", "U2", "s3cret", "s3cret", 1_000)
            .unwrap(),
        BindOutcome::Bound
    );
}
