//! Bridge the cancellable HTTP body into the shared synchronous frame decoder.
use super::{ApiResult, Error, ErrorKind, REQUEST_TIMEOUT, error_response_body};
use crate::api::access::{AccessKey, access_tls_config};
use axum::body::{Body, to_bytes};
use bytes::{Buf, Bytes};
use hyper::client::conn::http1;
use hyper::{Request, header};
use hyper_util::rt::TokioIo;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::Duration;
use tokio::runtime::Runtime;
use tokio::sync::{mpsc as async_mpsc, oneshot};
use tokio_stream::StreamExt;
use url::{Host, Position, Url};

pub(super) struct Reader {
    demand: async_mpsc::Sender<()>,
    chunks: Mutex<mpsc::Receiver<io::Result<Bytes>>>,
    buffered: Bytes,
    // Dropping the sender interrupts network I/O even with no active read.
    _stop: oneshot::Sender<()>,
}

impl io::Read for Reader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        while self.buffered.is_empty() {
            if self.demand.try_send(()).is_err() {
                return Ok(0);
            }
            let result = self
                .chunks
                .get_mut()
                .map_err(|_| io::Error::other("stream channel poisoned"))?
                .recv();
            let Ok(chunk) = result else { return Ok(0) };
            self.buffered = chunk?;
        }
        let count = output.len().min(self.buffered.len());
        self.buffered.copy_to_slice(&mut output[..count]);
        Ok(count)
    }
}

pub(super) fn open(
    url: Url,
    accept: &'static str,
    key: Option<AccessKey>,
    cancel: Arc<AtomicBool>,
) -> ApiResult<Option<Reader>> {
    if cancel.load(Ordering::Acquire) {
        return Ok(None);
    }
    let (stop, stopped) = oneshot::channel();
    let (demand, mut requests) = async_mpsc::channel(1);
    let (deliver, chunks) = mpsc::channel();
    let (ready, opened) = mpsc::channel();
    let flag = Arc::clone(&cancel);
    stream_runtime()?.spawn(async move {
        tokio::select! {
            biased;
            _ = stopped => {},
            _ = async {
                while !flag.load(Ordering::Acquire) {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            } => {},
            _ = async move {
                let response = match tokio::time::timeout(
                    REQUEST_TIMEOUT, connect(url, accept, key),
                ).await {
                    Ok(result) => result,
                    Err(error) => Err(transport_error("stream opening timed out", error)),
                };
                let mut body = match response {
                    Ok(body) => body.into_data_stream(),
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return;
                    }
                };
                if ready.send(Ok(())).is_err() {
                    return;
                }
                while requests.recv().await.is_some() {
                    let Some(chunk) = body.next().await else { break };
                    if deliver.send(chunk.map_err(io::Error::other)).is_err() {
                        break;
                    }
                }
            } => {},
        }
    });

    match opened.recv() {
        Ok(result) => result.map(|()| {
            Some(Reader {
                demand,
                chunks: Mutex::new(chunks),
                buffered: Bytes::new(),
                _stop: stop,
            })
        }),
        Err(_) if cancel.load(Ordering::Acquire) => Ok(None),
        Err(error) => Err(transport_error("stream could not open", error)),
    }
}

// The synchronous reader may run inside any caller runtime. A dedicated
// worker drives the outstanding HTTP bodies without a thread per reader.
fn stream_runtime() -> ApiResult<&'static Runtime> {
    static RUNTIME: OnceLock<Result<Runtime, io::Error>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .thread_name("plasmite-http-read")
                .enable_all()
                .build()
        })
        .as_ref()
        .map_err(|error| {
            Error::new(ErrorKind::Io)
                .with_message(format!("could not start stream runtime: {error}"))
        })
}

async fn connect(url: Url, accept: &'static str, key: Option<AccessKey>) -> ApiResult<Body> {
    let host = match url.host().expect("validated server URL") {
        Host::Domain(name) => name.to_owned(),
        Host::Ipv4(ip) => ip.to_string(),
        Host::Ipv6(ip) => ip.to_string(),
    };
    let address = (
        host.as_str(),
        url.port_or_known_default().expect("HTTP port"),
    );
    let socket = tokio::net::TcpStream::connect(address)
        .await
        .map_err(|error| transport_error("could not connect to stream server", error))?;
    let mut request = Request::get(&url[Position::BeforePath..Position::AfterQuery])
        .header(
            header::HOST,
            &url[Position::BeforeHost..Position::AfterPort],
        )
        .header(header::ACCEPT, accept)
        .header(header::CONNECTION, "close");
    let response = if url.scheme() == "https" {
        let pin = key.as_ref().map(|key| *key.spki_fingerprint());
        if let Some(key) = key {
            request = request.header(
                header::AUTHORIZATION,
                format!("Bearer {}", key.secret_hex()),
            );
        }
        let name = rustls::pki_types::ServerName::try_from(host)
            .map_err(|error| transport_error("invalid TLS server name", error))?;
        let connector = tokio_rustls::TlsConnector::from(Arc::new(access_tls_config(pin)));
        let socket = connector
            .connect(name, socket)
            .await
            .map_err(|error| transport_error("could not verify the stream server", error))?;
        send(socket, request).await?
    } else {
        send(socket, request).await?
    };
    let status = response.status();
    let version_is_2 = response
        .headers()
        .get("plasmite-version")
        .is_some_and(|value| value == "2");
    let body = Body::new(response.into_body());
    if status.is_success() {
        return Ok(body);
    }
    if status.as_u16() == 404 && !version_is_2 {
        return Err(Error::new(ErrorKind::Io)
            .with_message("server does not support Plasmite HTTP API v2")
            .with_hint("Upgrade the server to Plasmite 2.0."));
    }
    let bytes = to_bytes(body, 1024 * 1024).await.unwrap_or_default();
    Err(error_response_body(status.as_u16(), &bytes))
}

async fn send(
    socket: impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static,
    request: axum::http::request::Builder,
) -> ApiResult<hyper::Response<hyper::body::Incoming>> {
    let (mut sender, connection) = http1::handshake(TokioIo::new(socket))
        .await
        .map_err(|error| transport_error("could not open HTTP stream", error))?;
    tokio::spawn(connection);
    let request = request
        .body(Body::empty())
        .map_err(|error| transport_error("could not construct stream request", error))?;
    sender
        .send_request(request)
        .await
        .map_err(|error| transport_error("stream request failed", error))
}

fn transport_error(message: &str, error: impl std::error::Error + Send + Sync + 'static) -> Error {
    Error::new(ErrorKind::Io)
        .with_message(message)
        .with_source(error)
}
