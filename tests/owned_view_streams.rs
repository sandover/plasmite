use plasmite::api::{ErrorKind, Feed, Pool, PoolOptions, Read, Since};
use plasmite::view::MessageView;
use serde::Deserialize;
use std::sync::{Arc, atomic::AtomicBool};

#[test]
fn fetch_view_filters_and_retains_each_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("views.plasmite");
    let mut pool = Pool::create(&path, PoolOptions::new(65536)).unwrap();
    for (name, tag) in [("skip", "other"), ("first", "α\0β"), ("second", "α\0β")] {
        let view = MessageView::encode(
            &serde_json::json!({"name":name,"exact":i64::MAX,"negative":-0.0}),
            &[tag],
        )
        .unwrap();
        pool.feed_view(&view, Feed::default()).unwrap();
    }
    let retained = pool
        .fetch_view(Read::default().tags(["α\0β"]))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(retained.iter().map(|v| v.seq).collect::<Vec<_>>(), [2, 3]);
    // Overwrite and drop the producer; each retained snapshot stays independent.
    for _ in 0..500 {
        pool.feed(
            &serde_json::json!({"padding":"x".repeat(500)}),
            Feed::default(),
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
    assert_ne!(retained[0].bytes().as_ptr(), retained[1].bytes().as_ptr());
}

#[test]
fn view_read_reports_gaps_cancels_and_handles_empty_fetch() {
    let directory = tempfile::tempdir().unwrap();
    let mut pool = Pool::create(
        directory.path().join("policy.plasmite"),
        PoolOptions::new(16384),
    )
    .unwrap();
    for _ in 0..100 {
        pool.feed(
            &serde_json::json!({"padding":"x".repeat(300)}),
            Feed::default(),
        )
        .unwrap();
    }
    assert!(
        matches!(pool.follow_view(Read::since(Since::Seq(1))), Err(e) if e.kind() == ErrorKind::RetentionGap)
    );
    let cancel = Arc::new(AtomicBool::new(true));
    let mut cancelled = pool.follow_view(Read::default().cancel(cancel)).unwrap();
    assert!(cancelled.next_view().unwrap().is_none());
    let mut empty = pool.fetch_view(Read::last(0)).unwrap();
    assert!(empty.next_view().unwrap().is_none());
    assert!(
        pool.fetch_view(Read::default())
            .unwrap()
            .next_view()
            .unwrap()
            .unwrap()
            .seq
            > 1
    );
}
