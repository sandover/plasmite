// Multi-process lock smoke test for append serialization.
use std::process::{Command, Stdio};

use plasmite::core::pool::Pool;

fn cmd() -> Command {
    let exe = env!("CARGO_BIN_EXE_plasmite");
    Command::new(exe)
}

#[test]
fn concurrent_poke_is_serialized() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pool_dir = temp.path().join("pools");

    let create = cmd()
        .args(["--dir", pool_dir.to_str().unwrap(), "pool", "create", "lockpool"])
        .output()
        .expect("create");
    assert!(create.status.success());

    let workers = 8;
    let mut children = Vec::new();
    for i in 0..workers {
        let child = cmd()
            .args([
                "--dir",
                pool_dir.to_str().unwrap(),
                "poke",
                "lockpool",
                "--descrip",
                "lock",
                "--data-json",
                &format!("{{\"i\":{i}}}"),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn");
        children.push(child);
    }

    for mut child in children {
        let status = child.wait().expect("wait");
        assert!(status.success());
    }

    let pool_path = pool_dir.join("lockpool.plasmite");
    let pool = Pool::open(&pool_path).expect("open");
    let bounds = pool.bounds().expect("bounds");
    assert_eq!(bounds.oldest_seq, Some(1));
    assert_eq!(bounds.newest_seq, Some(workers as u64));
}
