//! Share the platform write wait across the live readers of one pool file.

#[cfg(test)]
use plasmite::api::internal::LocalPoolTransport;
use plasmite::api::notify::{PoolSubscription, SharedPoolWaiters};
use plasmite::api::{Error, ErrorKind, Pool};
use std::sync::Arc;
use tokio::sync::{Semaphore, watch};

#[derive(Clone)]
pub(crate) struct PoolWaiters {
    sources: SharedPoolWaiters,
    reads: Arc<Semaphore>,
}

impl Default for PoolWaiters {
    fn default() -> Self {
        let workers =
            std::thread::available_parallelism().map_or(2, |available| available.get().clamp(2, 8));
        Self {
            sources: SharedPoolWaiters::default(),
            reads: Arc::new(Semaphore::new(workers)),
        }
    }
}

pub(crate) struct PoolChange {
    changes: watch::Receiver<()>,
    _subscription: PoolSubscription,
}

impl PoolChange {
    /// Call before reading. A write during the read remains visible to changed().
    pub(crate) fn acknowledge(&mut self) {
        self.changes.borrow_and_update();
    }

    pub(crate) async fn changed(&mut self) {
        let _ = self.changes.changed().await;
    }
}

impl PoolWaiters {
    pub(crate) fn subscribe(&self, pool: &Pool) -> Result<PoolChange, Error> {
        self.register(pool, || Pool::open(pool.path()).map(Arc::new))
            .map(|(_, changes)| changes)
    }

    pub(crate) fn subscribe_owned(&self, pool: Pool) -> Result<(Arc<Pool>, PoolChange), Error> {
        let pool = Arc::new(pool);
        self.register(&pool, || Ok(pool.clone()))
    }

    fn register(
        &self,
        pool: &Pool,
        open: impl FnOnce() -> Result<Arc<Pool>, Error>,
    ) -> Result<(Arc<Pool>, PoolChange), Error> {
        let (send, changes) = watch::channel(());
        let (pool, subscription) = self.sources.subscribe(pool, open, move |_| {
            send.send_replace(());
        })?;
        Ok((
            pool,
            PoolChange {
                changes,
                _subscription: subscription,
            },
        ))
    }

    // Live turns wait for a worker without taking a short-request permit.
    pub(crate) async fn read<T>(
        &self,
        operation: impl FnOnce() -> Result<T, Error> + Send + 'static,
    ) -> Result<T, Error>
    where
        T: Send + 'static,
    {
        let permit = self
            .reads
            .clone()
            .acquire_owned()
            .await
            .expect("pool read permits");
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            operation()
        })
        .await
        .map_err(|err| {
            Error::new(ErrorKind::Internal)
                .with_message("pool read task failed")
                .with_source(err)
        })?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasmite::api::{
        Feed, HostedRead, LocalClient, PoolOptions, PoolRef, Read, ReadStatus, Since,
    };
    use std::time::Duration;

    #[tokio::test]
    async fn shares_a_notification_and_retires_after_the_last_reader() {
        let dir = tempfile::tempdir().unwrap();
        let client = LocalClient::new().with_pool_dir(dir.path());
        let name = PoolRef::name("watched");
        let mut pool = client.create_pool(&name, PoolOptions::new(65536)).unwrap();
        let waiters = PoolWaiters::default();
        let mut first = waiters.subscribe(&pool).unwrap();
        let mut second = waiters.subscribe(&pool).unwrap();
        assert_eq!(waiters.sources.pool_count(), 1);
        first.acknowledge();
        second.acknowledge();
        pool.feed_json(b"{}", Feed::new()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            first.changed().await;
            second.changed().await;
        })
        .await
        .unwrap();
        drop(first);
        assert_eq!(waiters.sources.pool_count(), 1);
        drop(second);
        tokio::time::timeout(Duration::from_secs(2), async {
            while waiters.sources.pool_count() != 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn a_write_after_the_read_remains_visible_to_changed() {
        let dir = tempfile::tempdir().unwrap();
        let client = LocalClient::new().with_pool_dir(dir.path());
        let name = PoolRef::name("fallback");
        let mut writer = client.create_pool(&name, PoolOptions::new(65536)).unwrap();
        let waiters = PoolWaiters::default();
        let (pool, mut changes) = waiters
            .subscribe_owned(client.open_pool(&name, false).unwrap())
            .unwrap();
        let mut reader = HostedRead::new(pool, Read::new()).unwrap();
        changes.acknowledge();
        assert!(matches!(reader.turn(&mut 32).unwrap(), ReadStatus::Waiting));
        writer.feed_json(b"{}", Feed::new()).unwrap();
        // The write lands after the read, before the watcher awaits.
        tokio::time::timeout(Duration::from_secs(2), changes.changed())
            .await
            .unwrap();
        let ReadStatus::Message(frame) = reader.turn(&mut 32).unwrap() else {
            panic!("the write must remain visible when it precedes the wait");
        };
        assert_eq!(frame.seq, 1);
    }

    #[tokio::test]
    async fn a_replaced_pool_gets_its_own_waiter() {
        let dir = tempfile::tempdir().unwrap();
        let client = LocalClient::new().with_pool_dir(dir.path());
        let name = PoolRef::name("replaceable");
        let pool = client.create_pool(&name, PoolOptions::new(65536)).unwrap();
        let waiters = PoolWaiters::default();
        let (old, mut changes) = waiters.subscribe_owned(pool).unwrap();
        let mut old_reader = HostedRead::new(old.clone(), Read::new()).unwrap();
        let identity = old.incarnation().unwrap();
        client.delete_pool(&name).unwrap();
        let new = client.create_pool(&name, PoolOptions::new(65536)).unwrap();
        let (new, _changes) = waiters.subscribe_owned(new).unwrap();
        assert_ne!(new.incarnation().unwrap(), identity);
        tokio::time::timeout(Duration::from_secs(2), changes.changed())
            .await
            .unwrap();
        assert!(
            matches!(old_reader.turn(&mut 32), Err(error) if error.kind() == ErrorKind::NotFound)
        );
    }

    #[test]
    fn a_read_turn_yields_after_examining_its_share_of_filtered_records() {
        let dir = tempfile::tempdir().unwrap();
        let client = LocalClient::new().with_pool_dir(dir.path());
        let name = PoolRef::name("filtered");
        let mut pool = client.create_pool(&name, PoolOptions::new(65536)).unwrap();
        for _ in 0..32 {
            pool.feed_json(b"{}", Feed::new()).unwrap();
        }
        pool.feed_json(b"{}", Feed::new().tags(["keep"])).unwrap();
        let mut reader =
            HostedRead::new(Arc::new(pool), Read::since(Since::Oldest).tags(["keep"])).unwrap();
        let mut budget = 32;
        assert!(matches!(
            reader.turn(&mut budget).unwrap(),
            ReadStatus::Waiting
        ));
        assert_eq!(budget, 0);
        assert_eq!(reader.position(), Some(33));
        let ReadStatus::Message(frame) = reader.turn(&mut 32).unwrap() else {
            panic!("the next turn should reach the matching record");
        };
        assert_eq!(frame.seq, 33);
    }
}
