// Controlled consumer benchmark: pass a scratch directory, then optional copied-baseline.
use plasmite::api::{Lite3DocRef, Pool, PoolApiExt, PoolOptions, lite3::Lite3Buf};
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

fn checksum(bytes: &[u8]) -> u64 {
    black_box(bytes)
        .iter()
        .fold(0u64, |sum, &b| sum.wrapping_add(u64::from(b)))
}
fn consume(pool: &Pool, mode: &str, copied_baseline: bool) {
    if mode == "full_json_decode" {
        let msg = pool.get_message(black_box(1)).expect("decode message");
        black_box((
            msg.seq,
            checksum(msg.data["blob"].as_str().expect("blob").as_bytes()),
            msg.meta.tags.len(),
            msg.time,
        ));
        return;
    }
    let frame = pool.get(black_box(1)).expect("get");
    match mode {
        "metadata" => {
            black_box((
                frame.seq,
                frame.timestamp_ns,
                frame.payload.len(),
                frame.payload[0],
            ));
        }
        "full_payload_checksum" => {
            black_box(checksum(&frame.payload));
        }
        "owned_payload_checksum" => {
            if copied_baseline {
                let owned = frame.payload.to_vec();
                black_box(checksum(&owned));
            } else {
                black_box(checksum(&frame.payload));
            }
        }
        "typed_fields" => {
            let doc = Lite3DocRef::new(&frame.payload);
            let ofs = doc.key_offset("data").expect("data");
            black_box((
                doc.i64_at_key(ofs, "sent_ns").expect("sent_ns"),
                doc.bool_at_key(ofs, "done").expect("done"),
            ));
        }
        _ => panic!("unknown mode"),
    }
}
fn row(
    mode: &str,
    size: usize,
    samples: &mut [u64],
    elapsed: Duration,
    writes: u64,
    write_seconds: f64,
) {
    samples.sort_unstable();
    let at = |fraction: f64| samples[((samples.len() - 1) as f64 * fraction) as usize];
    println!(
        "{{\"lane\":\"{mode}\",\"application_blob_bytes\":{size},\"reads\":{},\"seconds\":{},\"reads_per_sec\":{},\"p50_ns\":{},\"p95_ns\":{},\"p99_ns\":{},\"max_ns\":{},\"writes\":{writes},\"write_seconds\":{write_seconds}}}",
        samples.len(),
        elapsed.as_secs_f64(),
        samples.len() as f64 / elapsed.as_secs_f64(),
        at(0.50),
        at(0.95),
        at(0.99),
        samples[samples.len() - 1]
    );
}
fn main() {
    let mut args = std::env::args().skip(1);
    let directory = args.next().expect("work directory");
    let copied_baseline = args.next().as_deref() == Some("copied-baseline");
    std::fs::create_dir_all(&directory).expect("mkdir");
    for size in [256, 1024, 65536] {
        let json = format!(
            "{{\"meta\":{{\"tags\":[\"event\"]}},\"data\":{{\"blob\":\"{}\",\"sent_ns\":42,\"done\":false}}}}",
            "x".repeat(size)
        );
        let payload = Lite3Buf::from_json_str(&json)
            .expect("encode")
            .as_slice()
            .to_vec();
        for mode in [
            "metadata",
            "full_payload_checksum",
            "owned_payload_checksum",
            "typed_fields",
            "full_json_decode",
        ] {
            let path = std::path::Path::new(&directory).join(format!("{size}-{mode}.plasmite"));
            let mut writer = Pool::create(
                &path,
                PoolOptions::new(128 * 1024 * 1024).with_index_capacity(32768),
            )
            .expect("create");
            writer.append(&payload).expect("seed");
            let reader = Pool::open(&path).expect("reader");
            consume(&reader, mode, copied_baseline);
            let iterations = if size == 65536 { 10000 } else { 100000 };
            let mut samples = Vec::with_capacity(iterations);
            let start = Instant::now();
            for _ in 0..iterations {
                let read_start = Instant::now();
                consume(&reader, mode, copied_baseline);
                samples.push(read_start.elapsed().as_nanos() as u64);
            }
            row(
                &format!("idle_{mode}"),
                size,
                &mut samples,
                start.elapsed(),
                0,
                0.0,
            );
            let done = Arc::new(AtomicBool::new(false));
            let done_writer = Arc::clone(&done);
            let barrier = Arc::new(Barrier::new(2));
            let writer_barrier = Arc::clone(&barrier);
            let writes = if size == 65536 { 1000 } else { 10000 };
            let writer_payload = payload.clone();
            let thread = std::thread::spawn(move || {
                writer_barrier.wait();
                let start = Instant::now();
                for _ in 0..writes {
                    writer.append(&writer_payload).expect("write");
                }
                let elapsed = start.elapsed();
                done_writer.store(true, Ordering::Release);
                elapsed
            });
            samples.clear();
            barrier.wait();
            let start = Instant::now();
            while !done.load(Ordering::Acquire) {
                let read_start = Instant::now();
                consume(&reader, mode, copied_baseline);
                samples.push(read_start.elapsed().as_nanos() as u64);
            }
            let elapsed = start.elapsed();
            let write_elapsed = thread.join().expect("join");
            assert!(!samples.is_empty());
            row(
                &format!("contended_{mode}"),
                size,
                &mut samples,
                elapsed,
                writes,
                write_elapsed.as_secs_f64(),
            );
            drop(reader);
            std::fs::remove_file(path).expect("remove");
        }
    }
}
