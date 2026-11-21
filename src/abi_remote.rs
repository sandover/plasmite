//! Remote storage and bounded reads behind the native ABI.
use crate::api::internal::{LocalPoolTransport, RemotePoolTransport};
use crate::api::{Error, ErrorKind, Feed, FeedReceipt, FrameRef, Pool, RemotePool, TailOutcome};
use crate::core::pool::Durability;
use crate::view::MessageView;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

pub(crate) enum PoolHandle {
    Local(Box<Pool>),
    Remote(RemotePool),
}

impl From<Pool> for PoolHandle {
    fn from(pool: Pool) -> Self {
        Self::Local(Box::new(pool))
    }
}

impl PoolHandle {
    pub(crate) fn info(&self) -> Result<crate::api::PoolInfo, Error> {
        match self {
            Self::Local(pool) => pool.info(),
            Self::Remote(pool) => pool.info(),
        }
    }

    pub(crate) fn set_description(
        &mut self,
        description: crate::api::PoolDescription,
    ) -> Result<(), Error> {
        match self {
            Self::Local(pool) => pool.set_description(description),
            Self::Remote(pool) => pool.set_description(description),
        }
    }

    pub(crate) fn append_prepared(
        &mut self,
        bytes: &[u8],
        durability: Durability,
    ) -> Result<FeedReceipt, Error> {
        match self {
            Self::Local(pool) => pool.append_prepared(bytes, durability),
            Self::Remote(pool) => {
                pool.feed_lite3_receipt(bytes, Feed::new().durability(durability))
            }
        }
    }

    pub(crate) fn feed_lite3(
        &mut self,
        bytes: &[u8],
        feed: Feed,
    ) -> Result<crate::api::MessageEnvelope, Error> {
        match self {
            Self::Local(pool) => pool.feed_lite3(bytes, feed),
            Self::Remote(pool) => pool.feed_lite3(bytes, feed),
        }
    }

    pub(crate) fn feed_lite3_receipt(
        &mut self,
        bytes: &[u8],
        feed: Feed,
    ) -> Result<FeedReceipt, Error> {
        match self {
            Self::Local(pool) => pool.feed_lite3_receipt(bytes, feed),
            Self::Remote(pool) => pool.feed_lite3_receipt(bytes, feed),
        }
    }

    pub(crate) fn feed_prepared_view(
        &mut self,
        view: MessageView,
        durability: Durability,
    ) -> Result<MessageView, Error> {
        match self {
            Self::Local(pool) => pool.feed_prepared_view(view, durability),
            Self::Remote(pool) => pool.feed_committed_view(view.bytes(), durability),
        }
    }

    pub(crate) fn feed_view(
        &mut self,
        view: &MessageView,
        feed: Feed,
    ) -> Result<MessageView, Error> {
        match self {
            Self::Local(pool) => pool.feed_view(view, feed),
            Self::Remote(pool) => {
                pool.feed_committed_view(view.bytes(), feed.encoded_durability()?)
            }
        }
    }
}

// The HTTP decoder can be partway through a frame when a caller's wait expires.
// Keep that read in flight, and resume waiting for its result on the next call.
pub(super) struct RemoteFrames {
    request: mpsc::Sender<()>,
    replies: mpsc::Receiver<Result<Option<FrameRef>, Error>>,
    cancel: Arc<AtomicBool>,
    pending: bool,
    staged: Option<FrameRef>,
    complete: bool,
    failure: Option<Error>,
}

impl RemoteFrames {
    pub(super) fn start(
        mut reader: crate::api::RemoteLite3Read,
        cancel: Arc<AtomicBool>,
    ) -> Result<Self, Error> {
        let (request, requested) = mpsc::channel();
        let (reply, replies) = mpsc::channel();
        std::thread::Builder::new()
            .name("plasmite-native-http-reader".into())
            .spawn(move || {
                while requested.recv().is_ok() {
                    let result = reader.next_frame().map(|frame| {
                        frame.map(|frame| FrameRef {
                            seq: frame.seq,
                            time: frame.time,
                            flags: 0,
                            payload: frame.payload,
                        })
                    });
                    let finished = !matches!(result, Ok(Some(_)));
                    if reply.send(result).is_err() || finished {
                        break;
                    }
                }
            })
            .map_err(|err| {
                Error::new(ErrorKind::Io)
                    .with_message("could not start remote reader")
                    .with_source(err)
            })?;
        Ok(Self {
            request,
            replies,
            cancel,
            pending: false,
            staged: None,
            complete: false,
            failure: None,
        })
    }

    pub(super) fn next<T>(
        &mut self,
        wait: Option<Duration>,
        max_bytes: usize,
        select: impl Fn(FrameRef, &[String]) -> Result<Option<T>, Error>,
    ) -> Result<TailOutcome<T>, Error> {
        if let Some(error) = self.failure.as_ref() {
            return Err(error.clone());
        }
        if self.cancel.load(Ordering::Acquire) {
            return Ok(TailOutcome::Cancelled);
        }
        if self.complete {
            return Ok(TailOutcome::Finished);
        }
        if self.staged.is_none() {
            if !self.pending {
                self.request.send(()).map_err(|_| disconnected())?;
                self.pending = true;
            }
            let result = match wait {
                Some(wait) => self.replies.recv_timeout(wait),
                None => self
                    .replies
                    .recv()
                    .map_err(|_| mpsc::RecvTimeoutError::Disconnected),
            };
            self.staged = match result {
                Ok(Ok(Some(frame))) => Some(frame),
                Ok(Ok(None)) if self.cancel.load(Ordering::Acquire) => {
                    return Ok(TailOutcome::Cancelled);
                }
                Ok(Ok(None)) => {
                    self.complete = true;
                    return Ok(TailOutcome::Finished);
                }
                Ok(Err(error)) => return Err(self.remember(error)),
                Err(mpsc::RecvTimeoutError::Timeout) => return Ok(TailOutcome::WaitExpired),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(self.remember(disconnected()));
                }
            };
            self.pending = false;
        }

        let frame = self.staged.as_ref().expect("received frame");
        if frame.payload.len() > max_bytes {
            return Err(Error::new(ErrorKind::Usage).with_message(format!(
                "next frame needs {} encoded bytes, above batch budget {max_bytes}",
                frame.payload.len()
            )));
        }
        let frame = self.staged.take().expect("received frame");
        match select(frame, &[]) {
            Ok(Some(item)) => Ok(TailOutcome::Item(item)),
            Ok(None) => Err(self.remember(
                Error::new(ErrorKind::Internal)
                    .with_message("remote reader returned a filtered record"),
            )),
            Err(error) => Err(self.remember(error)),
        }
    }

    fn remember(&mut self, error: Error) -> Error {
        self.failure = Some(error.clone());
        error
    }
}

impl Drop for RemoteFrames {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

fn disconnected() -> Error {
    Error::new(ErrorKind::Io).with_message("remote reader stopped without an ending")
}
