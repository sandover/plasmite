//! A stream keeps one parked native waiter across reads. The C API waits on one
//! pool address/socket at a time; separate waiters avoid rotating idle handles.
//! Closing drops the sender, cancels the active read, and lets its thread exit.
use std::sync::mpsc::{self, Sender};

type Job = Box<dyn FnMut(u64) -> bool + Send>;
#[derive(Clone)]
pub(super) struct Worker(Sender<Job>);
impl Worker {
    pub(super) fn new() -> std::io::Result<Self> {
        let (send, receive) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("plasmite-wait".into())
            .spawn(move || {
                while let Ok(mut job) = receive.recv() {
                    while !job(50) {}
                }
            })?;
        Ok(Self(send))
    }
    pub(super) fn submit(&self, job: Job) {
        self.0
            .send(job)
            .unwrap_or_else(|_| panic!("stream wait worker stopped unexpectedly"));
    }
}

// Probe ready/backlogged reads on a small shared executor. Only an empty read
// moves to its own parked waiter; later demand starts with a fresh probe.
struct Probe {
    read: Job,
    waiter: Option<Worker>,
}
pub(super) fn submit(waiter: Option<Worker>, probe: bool, read: Job) {
    // Network arrival is independent of the local producer backlog. Avoid a
    // failed socket-mode-changing poll before its native blocking wait.
    if !probe {
        waiter
            .expect("positive socket wait owns waiter")
            .submit(read);
        return;
    }
    use std::sync::{Arc, Mutex, OnceLock};
    static PROBES: OnceLock<Sender<Probe>> = OnceLock::new();
    let queue = PROBES.get_or_init(|| {
        let (send, receive) = mpsc::channel::<Probe>();
        let receive = Arc::new(Mutex::new(receive));
        for index in 0..4 {
            let receive = Arc::clone(&receive);
            std::thread::Builder::new()
                .name(format!("plasmite-read-{index}"))
                .spawn(move || {
                    loop {
                        let request = receive.lock().unwrap().recv();
                        let Ok(mut request) = request else {
                            break;
                        };
                        if !(request.read)(0) {
                            request
                                .waiter
                                .expect("positive wait owns waiter")
                                .submit(request.read);
                        }
                    }
                })
                .expect("start native read executor");
        }
        send
    });
    queue
        .send(Probe { read, waiter })
        .unwrap_or_else(|_| panic!("native read executor stopped unexpectedly"));
}
