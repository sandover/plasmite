//! Share one native write wait across the listeners on an opened pool file.

use super::{NotifyWait, open_for_pool};
use crate::api::{Error, ErrorKind, Pool};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

type Callback = dyn Fn(bool) + Send + Sync;
type Sources = Arc<Mutex<HashMap<String, Source>>>;

struct Source {
    pool: Arc<Pool>,
    listeners: Vec<Weak<Callback>>,
}

/// Keep a callback registered for as long as its caller needs change signals.
pub struct PoolSubscription {
    _callback: Arc<Callback>,
}

#[derive(Clone, Default)]
pub struct SharedPoolWaiters {
    sources: Sources,
}

impl SharedPoolWaiters {
    /// Call `wake(false)` on progress and `wake(true)` if the opened file disappears.
    pub fn subscribe(
        &self,
        pool: &Pool,
        open: impl FnOnce() -> Result<Arc<Pool>, Error>,
        wake: impl Fn(bool) + Send + Sync + 'static,
    ) -> Result<(Arc<Pool>, PoolSubscription), Error> {
        let identity = pool.incarnation()?;
        let callback: Arc<Callback> = Arc::new(wake);
        let listener = Arc::downgrade(&callback);
        let subscription = PoolSubscription {
            _callback: callback,
        };
        if let Some(source) = self
            .sources
            .lock()
            .expect("pool wait sources")
            .get_mut(&identity)
        {
            source.listeners.push(listener);
            return Ok((source.pool.clone(), subscription));
        }

        let observer = open()?;
        if observer.incarnation()? != identity {
            return Err(Error::new(ErrorKind::NotFound).with_message("pool was replaced"));
        }
        // Check progress before registering so the recovery check sees writes
        // that land while we open the native wait handle.
        let newest = observer.bounds()?.newest_seq;
        let notify = open_for_pool(&observer);
        // Opening a pool and taking its bounds can wait for the file lock.
        // Another subscriber may have started this waiter while we did that.
        let mut sources = self.sources.lock().expect("pool wait sources");
        if let Some(source) = sources.get_mut(&identity) {
            source.listeners.push(listener);
            return Ok((source.pool.clone(), subscription));
        }
        let worker_pool = observer.clone();
        let registry = self.sources.clone();
        let worker_identity = identity.clone();
        std::thread::Builder::new()
            .name("plasmite-pool-wait".into())
            .spawn(move || {
                let mut newest = newest;
                let mut notify = notify;
                loop {
                    {
                        let mut sources = registry.lock().expect("pool wait sources");
                        let source = sources.get_mut(&worker_identity).expect("worker's pool");
                        source.listeners.retain(|wake| wake.strong_count() != 0);
                        if source.listeners.is_empty() {
                            sources.remove(&worker_identity);
                            return;
                        }
                    }
                    match notify
                        .as_mut()
                        .map(|handle| handle.wait(Duration::from_secs(1)))
                    {
                        Some(NotifyWait::Signaled | NotifyWait::TimedOut) => {}
                        Some(NotifyWait::Unavailable) | None => {
                            notify = None;
                            std::thread::sleep(Duration::from_millis(50));
                        }
                    }
                    let mut removed = false;
                    match worker_pool
                        .check_current_path()
                        .and_then(|()| worker_pool.bounds())
                    {
                        Ok(bounds) if newest == bounds.newest_seq => continue,
                        Ok(bounds) => newest = bounds.newest_seq,
                        Err(error) if error.kind() == ErrorKind::NotFound => removed = true,
                        Err(_) => std::thread::sleep(Duration::from_millis(50)),
                    }
                    let callbacks = {
                        let mut sources = registry.lock().expect("pool wait sources");
                        let callbacks = sources[&worker_identity]
                            .listeners
                            .iter()
                            .filter_map(Weak::upgrade)
                            .collect::<Vec<_>>();
                        if removed {
                            sources.remove(&worker_identity);
                        }
                        callbacks
                    };
                    for wake in callbacks {
                        wake(removed);
                    }
                    if removed {
                        return;
                    }
                }
            })
            .map_err(|err| {
                Error::new(ErrorKind::Io)
                    .with_message("could not start a pool waiter")
                    .with_source(err)
            })?;
        sources.insert(
            identity,
            Source {
                pool: observer.clone(),
                listeners: vec![listener],
            },
        );
        Ok((observer, subscription))
    }

    #[doc(hidden)]
    pub fn pool_count(&self) -> usize {
        self.sources.lock().expect("pool wait sources").len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{Feed, PoolOptions};
    use std::sync::mpsc;

    #[test]
    fn opening_one_pool_does_not_delay_a_callback_for_another() {
        let directory = tempfile::tempdir().unwrap();
        let first_path = directory.path().join("first.plasmite");
        let second_path = directory.path().join("second.plasmite");
        let first_pool = Pool::create(&first_path, PoolOptions::new(65536)).unwrap();
        let mut second_pool = Pool::create(&second_path, PoolOptions::new(65536)).unwrap();
        let waiters = SharedPoolWaiters::default();
        let (changed, received) = mpsc::channel();
        let (_, _subscription) = waiters
            .subscribe(
                &second_pool,
                || Pool::open(&second_path).map(Arc::new),
                move |_| {
                    let _ = changed.send(());
                },
            )
            .unwrap();
        let (opening, started) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let opener = std::thread::spawn(move || {
            waiters.subscribe(
                &first_pool,
                || {
                    opening.send(()).unwrap();
                    released.recv_timeout(Duration::from_secs(5)).unwrap();
                    Pool::open(&first_path).map(Arc::new)
                },
                |_| {},
            )
        });
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        second_pool
            .feed(&serde_json::json!({}), Feed::new())
            .unwrap();
        let result = received.recv_timeout(Duration::from_secs(2));
        release.send(()).unwrap();
        opener.join().unwrap().unwrap();
        result.expect("the second pool should notify while the first opens");
    }
}
