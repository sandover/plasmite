use plasmite::api::{
    AppendOptions, Durability, ErrorKind, GapPolicy, Pool, PoolApiExt, PoolOptions, TailOptions,
};
use serde::Deserialize;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

fn options() -> TailOptions {
    TailOptions {
        since_seq: Some(1),
        timeout: Some(Duration::from_millis(100)),
        notify: false,
        ..TailOptions::new()
    }
}

#[test]
fn view_tail_filters_counts_and_retains_each_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("views.plasmite");
    let mut pool = Pool::create(&path, PoolOptions::new(65536)).unwrap();
    for (name, tag) in [("skip", "other"), ("first", "α\0β"), ("second", "α\0β")] {
        pool.append_value(
            &serde_json::json!({"name":name,"exact":i64::MAX,"negative":-0.0}),
            &[tag],
            AppendOptions::new(123, Durability::Fast),
        )
        .unwrap();
    }
    let mut selected = options();
    selected.tags = vec!["α\0β".into()];
    selected.max_messages = Some(2);
    let retained = pool
        .tail_views(selected)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(retained.iter().map(|v| v.seq).collect::<Vec<_>>(), [2, 3]);
    // Overwrite and drop the producer; each retained snapshot stays independent.
    for _ in 0..500 {
        pool.append_value(
            &serde_json::json!({"padding":"x".repeat(500)}),
            &[],
            AppendOptions::new(456, Durability::Fast),
        )
        .unwrap();
    }
    drop(pool);
    #[derive(Deserialize)]
    struct Reading<'a> {
        name: &'a str,
        exact: i64,
        negative: f64,
    }
    let first: Reading = retained[0].data().decode().unwrap();
    assert_eq!(first.name, "first");
    assert_eq!(first.exact, i64::MAX);
    assert!(first.negative.is_sign_negative());
    assert_eq!(retained[0].timestamp_ns, 123);
    assert_ne!(retained[0].bytes().as_ptr(), retained[1].bytes().as_ptr());
}

#[test]
fn view_tail_reuses_gap_cancel_and_zero_limit_policy() {
    let directory = tempfile::tempdir().unwrap();
    let mut pool = Pool::create(
        directory.path().join("policy.plasmite"),
        PoolOptions::new(16384),
    )
    .unwrap();
    for _ in 0..100 {
        pool.append_value(
            &serde_json::json!({"padding":"x".repeat(300)}),
            &[],
            AppendOptions::new(1, Durability::Fast),
        )
        .unwrap();
    }
    let mut strict = options();
    strict.gap_policy = GapPolicy::Error;
    let mut tail = pool.tail_views(strict);
    assert!(matches!(tail.next_view(),Err(e) if e.kind()==ErrorKind::RetentionGap));
    assert!(tail.next_view().unwrap().is_none());
    let cancel = Arc::new(AtomicBool::new(true));
    let mut cancelled = options();
    cancelled.cancel = Some(cancel.clone());
    assert!(pool.tail_views(cancelled).next_view().unwrap().is_none());
    cancel.store(false, Ordering::Release);
    let mut empty = options();
    empty.max_messages = Some(0);
    assert!(pool.tail_views(empty).next_view().unwrap().is_none());
    let mut continued = options();
    continued.max_messages = Some(1);
    assert!(pool.tail_views(continued).next_view().unwrap().unwrap().seq > 1);
}
