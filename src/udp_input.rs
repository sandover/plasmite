//! Append one routed JSON or Lite3 message per UDP datagram.

use plasmite::api::{Durability, Error, ErrorKind, LocalClient, PoolApiExt, PoolRef};
use serde::Deserialize;
use serde_json::Value;
use std::net::SocketAddr;
use tokio::net::UdpSocket;
use tokio::sync::watch;
use tokio::time::{Duration, Instant};

pub(crate) const MAX_DATAGRAM_BYTES: usize = 65_507;

pub(crate) async fn bind(address: SocketAddr) -> Result<UdpSocket, Error> {
    let socket = UdpSocket::bind(address).await.map_err(|err| {
        Error::new(ErrorKind::Io)
            .with_message("failed to bind UDP input listener")
            .with_source(err)
    })?;
    // Some hosts default to a receive queue smaller than one maximum datagram.
    socket2::SockRef::from(&socket)
        .set_recv_buffer_size(128 * 1024)
        .map_err(|err| {
            Error::new(ErrorKind::Io)
                .with_message("failed to configure UDP receive buffer")
                .with_source(err)
        })?;
    Ok(socket)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonInput {
    data: Value,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
struct Counters {
    received: u64,
    appended: u64,
    oversized: u64,
    errors: u64,
}

impl Counters {
    fn report(self) {
        tracing::info!(
            received = self.received,
            appended = self.appended,
            oversized = self.oversized,
            errors = self.errors,
            "UDP input summary"
        );
    }
}

fn append(client: &LocalClient, mut packet: Vec<u8>) -> Result<(), Error> {
    let invalid = || Error::new(ErrorKind::Usage).with_message("invalid UDP message");
    let separator = packet
        .iter()
        .position(|byte| *byte == 0)
        .ok_or_else(invalid)?;
    let name = std::str::from_utf8(&packet[..separator])
        .map_err(|_| invalid())?
        .to_owned();
    let body = &packet[separator + 1..];
    let first = *body.first().ok_or_else(invalid)?;
    // Lite3 object documents begin with their object-root type byte.
    if first == 0x06 {
        // Lite3 readers require aligned storage. A variable-length routing prefix
        // can misalign the body, so move its unchanged bytes to the allocation start.
        let body_len = body.len();
        packet.copy_within(separator + 1.., 0);
        packet.truncate(body_len);
        let mut pool = client.open_pool(&PoolRef::name(name))?;
        pool.append_lite3_now(&packet, Durability::Fast)?;
    } else {
        let input: JsonInput = serde_json::from_slice(body).map_err(|_| invalid())?;
        if !input.data.is_object() {
            return Err(invalid());
        }
        let mut pool = client.open_pool(&PoolRef::name(name))?;
        pool.append_json_now(&input.data, &input.tags, Durability::Fast)?;
    }
    Ok(())
}

pub(crate) async fn run(
    socket: UdpSocket,
    client: LocalClient,
    shutdown: watch::Receiver<bool>,
) -> Result<(), Error> {
    // One complete ordinary datagram fits; even an IPv6 datagram above our cap
    // remains intact so we can discard it without accepting a truncated body.
    let mut buffer = [0u8; 65_536];
    let mut counts = Counters::default();
    let mut reported = counts;
    let mut interval = tokio::time::interval_at(
        Instant::now() + Duration::from_secs(30),
        Duration::from_secs(30),
    );
    let stop = crate::serve::wait_for_shutdown(shutdown);
    tokio::pin!(stop);
    let result = loop {
        let length = tokio::select! {
            biased;
            _ = &mut stop => break Ok(()),
            _ = interval.tick() => {
                if counts != reported { counts.report(); reported = counts; }
                continue;
            }
            received = socket.recv_from(&mut buffer) => match received {
                Ok((length, _)) => length,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => break Err(Error::new(ErrorKind::Io)
                    .with_message("UDP receive failed").with_source(error)),
            }
        };
        counts.received += 1;
        if length > MAX_DATAGRAM_BYTES {
            counts.oversized += 1;
            continue;
        }
        let packet = buffer[..length].to_vec();
        let client = client.clone();
        let mut pending = tokio::task::spawn_blocking(move || append(&client, packet));
        let mut stopping = false;
        let outcome = loop {
            tokio::select! {
                outcome = &mut pending => break outcome,
                _ = &mut stop, if !stopping => stopping = true,
                _ = interval.tick() => {
                    if counts != reported { counts.report(); reported = counts; }
                }
            }
        };
        match outcome {
            Ok(Ok(())) => counts.appended += 1,
            Ok(Err(_)) => counts.errors += 1,
            Err(error) => {
                counts.errors += 1;
                break Err(Error::new(ErrorKind::Internal)
                    .with_message("UDP append task failed")
                    .with_source(error));
            }
        }
        if stopping {
            break Ok(());
        }
    };
    counts.report();
    result
}
