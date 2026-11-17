//! Prepared-history consumer measurement. Allocation diagnostics run separately.
use plasmite::api::{
    AppendOptions, Durability, GapPolicy, Meta, Pool, PoolApiExt, PoolOptions, FollowOptions,
    lite3::encode_message,
};
use serde_json::{Value, json};
use std::alloc::{GlobalAlloc, Layout, System};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::time::Instant;

struct CountAlloc;
static TRACK: AtomicBool = AtomicBool::new(false);
static CALLS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);

fn record(size: usize) {
    if TRACK.load(Relaxed) {
        CALLS.fetch_add(1, Relaxed);
        BYTES.fetch_add(size as u64, Relaxed);
    }
}

unsafe impl GlobalAlloc for CountAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record(size);
        unsafe { System.realloc(ptr, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountAlloc = CountAlloc;
const BASE_NS: u64 = 1_700_000_000_000_000_000;

fn usage() -> libc::rusage {
    let mut result = std::mem::MaybeUninit::zeroed();
    assert_eq!(
        unsafe { libc::getrusage(libc::RUSAGE_SELF, result.as_mut_ptr()) },
        0
    );
    unsafe { result.assume_init() }
}

fn cpu(value: &libc::rusage) -> f64 {
    (value.ru_utime.tv_sec + value.ru_stime.tv_sec) as f64
        + (value.ru_utime.tv_usec + value.ru_stime.tv_usec) as f64 / 1_000_000.0
}

fn options(count: usize, mode: &str) -> FollowOptions {
    FollowOptions {
        start: plasmite::api::FollowStart::Sequence(1),
        max_messages: Some(count),
        notify: false,
        gap_policy: GapPolicy::Error,
        tags: if mode == "tags" {
            vec!["sensor".into()]
        } else {
            vec![]
        },
        ..FollowOptions::new()
    }
}

fn consume(pool: &Pool, mode: &str, count: usize, payload: &[u8], data: &Value) {
    if mode == "json" {
        let mut tail = pool.follow(options(count, mode));
        for seq in 1..=count as u64 {
            let message = tail.next_message().unwrap().unwrap();
            assert_eq!(message.seq, seq);
            assert_eq!(message.timestamp_ns().unwrap(), BASE_NS + seq);
            assert_eq!(message.meta.tags, ["sensor", "checked"]);
            assert_eq!(&message.data, data);
        }
        assert!(tail.next_message().unwrap().is_none());
    } else if mode == "get" || mode == "meta" {
        for seq in 1..=count as u64 {
            let frame = pool.get(seq).unwrap();
            assert_eq!(frame.timestamp_ns, BASE_NS + seq);
            assert_eq!(frame.seq, seq);
            assert_eq!(frame.payload, payload);
            if mode == "meta" {
                assert_eq!(
                    Meta::from_lite3(&frame.payload).unwrap().tags,
                    ["sensor", "checked"]
                );
            }
        }
    } else {
        assert!(mode == "raw" || mode == "tags");
        let mut tail = pool.follow_lite3(options(count, mode));
        for seq in 1..=count as u64 {
            let frame = tail.next_frame().unwrap().unwrap();
            assert_eq!(frame.seq, seq);
            assert_eq!(frame.timestamp_ns, BASE_NS + seq);
            assert_eq!(frame.payload, payload);
        }
        assert!(tail.next_frame().unwrap().is_none());
    }
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let action = &args[1];
    let path = Path::new(&args[2]);
    let fixture: Value = serde_json::from_slice(&std::fs::read(&args[3]).unwrap()).unwrap();
    let data = &fixture["data"];
    let count: usize = args[4].parse().unwrap();
    if action == "prepare" {
        let payload = encode_message(&["sensor".into(), "checked".into()], data).unwrap();
        let mut pool = Pool::create(
            path,
            PoolOptions::new((count * (payload.len() + 4096) + 65536) as u64)
                .with_index_capacity(count.next_power_of_two() as u32),
        )
        .unwrap();
        for seq in 1..=count as u64 {
            assert_eq!(
                pool.append_with_options(
                    payload.as_slice(),
                    AppendOptions::new(BASE_NS + seq, Durability::Fast)
                )
                .unwrap(),
                seq
            );
        }
        std::fs::write(path.with_extension("fixture"), payload.as_slice()).unwrap();
        println!(
            "{}",
            json!({"messages":count,"payload_bytes":payload.len()})
        );
        return;
    }
    let mode = &args[5];
    let diagnostic = args.get(6).is_some_and(|v| v == "alloc");
    let payload = std::fs::read(path.with_extension("fixture")).unwrap();
    let pool = Pool::open(path).unwrap();
    // Warm the complete mapping and allocator, with the same requested work.
    consume(&pool, mode, count, &payload, data);
    CALLS.store(0, Relaxed);
    BYTES.store(0, Relaxed);
    let before = usage();
    let started = Instant::now();
    TRACK.store(diagnostic, Relaxed);
    consume(&pool, mode, count, &payload, data);
    TRACK.store(false, Relaxed);
    let elapsed = started.elapsed();
    let after = usage();
    let rss_scale = if cfg!(target_os = "macos") { 1 } else { 1024 };
    println!(
        "{}",
        json!({
            "mode":mode,"messages":count,"verified_messages":count,
            "elapsed_ns":elapsed.as_nanos() as u64,"cpu_seconds":cpu(&after)-cpu(&before),
            "peak_rss_bytes":after.ru_maxrss as u64 * rss_scale,
            "rust_allocation_calls":if diagnostic { Some(CALLS.load(Relaxed)) } else { None },
            "rust_allocation_bytes_requested":if diagnostic { Some(BYTES.load(Relaxed)) } else { None },
        })
    );
}
