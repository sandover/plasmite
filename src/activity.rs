//! Current server requests and local processes with an open pool file.
//! This module keeps no client history and performs no network lookups.

use axum::body::Body;
use axum::http::{HeaderMap, header};
use axum::response::Response;
use hyper::body::{Body as HttpBody, Frame, SizeHint};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Instant;

#[derive(Clone, Default)]
pub(super) struct ActivityRegistry(Arc<Mutex<Registry>>);

#[derive(Default)]
struct Registry {
    next_id: u64,
    requests: BTreeMap<u64, (String, Instant, Entry)>,
}

#[derive(Clone, Serialize)]
struct Entry {
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    peer_ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    user_agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    operation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<String>,
}

#[derive(Clone)]
pub(super) struct RequestActivity(Arc<RequestObservation>);

struct RequestObservation {
    registry: ActivityRegistry,
    id: Mutex<Option<u64>>,
    entry: Entry,
}

impl ActivityRegistry {
    pub(super) fn request(
        &self,
        peer: Option<SocketAddr>,
        headers: &HeaderMap,
        kind: &'static str,
    ) -> RequestActivity {
        RequestActivity(Arc::new(RequestObservation {
            registry: self.clone(),
            id: Mutex::new(None),
            entry: Entry {
                kind,
                peer_ip: peer.map(|peer| peer.ip().to_string()),
                user_agent: headers
                    .get(header::USER_AGENT)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned),
                operation: None,
                pid: None,
                command: None,
            },
        }))
    }

    pub(super) fn snapshot(&self, pools: &[(String, PathBuf)]) -> Value {
        // lsof observes processes, including readers that bypass this server.
        // An open handle does not prove that a process follows or writes a pool.
        let local = local_open_files(pools);
        let mut result = BTreeMap::new();
        for (name, _) in pools {
            result.insert(
                name.clone(),
                json!({"entries": [], "local_files": {
                    "available": local.error.is_none(), "error": local.error
                }}),
            );
        }
        for (pool, entry) in local.entries {
            if let Some(pool) = result.get_mut(&pool) {
                pool["entries"].as_array_mut().unwrap().push(json!(entry));
            }
        }
        for (name, started, entry) in self.0.lock().unwrap().requests.values() {
            if let Some(pool) = result.get_mut(name) {
                let mut entry = json!(entry);
                entry["age_ms"] = json!(started.elapsed().as_millis());
                pool["entries"].as_array_mut().unwrap().push(entry);
            }
        }
        json!({"hostname": hostname(), "pools": result})
    }
}

impl RequestActivity {
    pub(super) fn pool(&self, name: String, operation: String) {
        let mut observation_id = self.0.id.lock().unwrap();
        if observation_id.is_some() {
            return;
        }
        let mut registry = self.0.registry.0.lock().unwrap();
        let id = registry.next_id;
        registry.next_id += 1;
        let mut entry = self.0.entry.clone();
        entry.operation = Some(operation);
        let name = name.strip_suffix(".plasmite").unwrap_or(&name).to_owned();
        registry.requests.insert(id, (name, Instant::now(), entry));
        *observation_id = Some(id);
    }

    pub(super) fn retain_for_response(self, response: Response) -> Response {
        let (parts, body) = response.into_parts();
        if body.is_end_stream() {
            return Response::from_parts(parts, body);
        }
        Response::from_parts(
            parts,
            Body::new(ObservedBody {
                body,
                activity: Some(self),
            }),
        )
    }
}

impl Drop for RequestObservation {
    fn drop(&mut self) {
        if let Some(id) = self.id.get_mut().unwrap().take() {
            self.registry.0.lock().unwrap().requests.remove(&id);
        }
    }
}

struct ObservedBody {
    body: Body,
    activity: Option<RequestActivity>,
}

impl HttpBody for ObservedBody {
    type Data = bytes::Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let frame = Pin::new(&mut self.body).poll_frame(cx);
        if matches!(frame, Poll::Ready(None | Some(Err(_)))) || self.body.is_end_stream() {
            self.activity.take();
        }
        frame
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.body.size_hint()
    }
}

struct LocalFiles {
    entries: Vec<(String, Entry)>,
    error: Option<String>,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn local_open_files(pools: &[(String, PathBuf)]) -> LocalFiles {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    const MAX_OUTPUT: u64 = 256 * 1024;
    let unavailable = |error: String| LocalFiles {
        entries: Vec::new(),
        error: Some(error),
    };
    if pools.is_empty() {
        return LocalFiles {
            entries: Vec::new(),
            error: None,
        };
    }
    let pools = match pools
        .iter()
        .map(|(name, path)| path.canonicalize().map(|path| (name.clone(), path)))
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(pools) => pools,
        Err(err) => return unavailable(format!("Cannot resolve pool files: {err}")),
    };
    let paths = pools.iter().map(|(_, path)| path);
    let mut child = match Command::new("lsof")
        .args(["-n", "-P", "+c", "0", "-F0pcn", "--"])
        .args(paths)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => return unavailable(format!("Cannot run lsof: {err}")),
    };
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let read = |pipe: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut output = Vec::new();
            pipe.take(MAX_OUTPUT + 1)
                .read_to_end(&mut output)
                .map(|_| output)
        })
    };
    let output = read(Box::new(stdout));
    let errors = read(Box::new(stderr));
    let deadline = Instant::now() + Duration::from_secs(1);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            result => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(match result {
                    Err(err) => format!("Cannot inspect lsof: {err}"),
                    _ => "lsof exceeded one second".to_owned(),
                });
            }
        }
    };
    let output = output.join().ok().and_then(Result::ok);
    let errors = errors.join().ok().and_then(Result::ok);
    let (Some(output), Some(errors)) = (output, errors) else {
        return unavailable("Cannot read lsof output".to_owned());
    };
    let status = match status {
        Ok(status) => status,
        Err(error) => return unavailable(error),
    };
    if output.len() as u64 > MAX_OUTPUT || errors.len() as u64 > MAX_OUTPUT {
        return unavailable("lsof output exceeded 256 KiB".to_owned());
    }
    // lsof exits with 1 if any selected file has no open handle, even when it
    // reports handles for other files. Diagnostics identify incomplete coverage.
    let error = if !errors.is_empty() {
        Some(format!("lsof: {}", String::from_utf8_lossy(&errors).trim()))
    } else if !(status.success() || status.code() == Some(1)) {
        Some(format!("lsof exited with {status}"))
    } else {
        None
    };
    LocalFiles {
        entries: parse_lsof(&output, &pools, std::process::id()),
        error,
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn local_open_files(_pools: &[(String, PathBuf)]) -> LocalFiles {
    LocalFiles {
        entries: Vec::new(),
        error: Some("Local file observation requires macOS or Linux".to_owned()),
    }
}

#[cfg(any(target_os = "macos", target_os = "linux", test))]
fn parse_lsof(output: &[u8], pools: &[(String, PathBuf)], server_pid: u32) -> Vec<(String, Entry)> {
    let paths: BTreeMap<_, _> = pools
        .iter()
        .map(|(name, path)| (path.to_string_lossy().into_owned(), name.clone()))
        .collect();
    let mut pid = None;
    let mut command = None;
    let mut entries = BTreeMap::new();
    for field in output.split(|byte| *byte == 0) {
        let field = field.strip_prefix(b"\n").unwrap_or(field);
        let Some((&tag, value)) = field.split_first() else {
            continue;
        };
        let value = String::from_utf8_lossy(value).into_owned();
        match tag {
            b'p' => {
                pid = value.parse::<u32>().ok();
                command = None;
            }
            b'c' => command = Some(value),
            b'n' => {
                if let (Some(pid), Some(pool)) =
                    (pid.filter(|pid| *pid != server_pid), paths.get(&value))
                {
                    entries.insert(
                        (pool.clone(), pid),
                        Entry {
                            kind: "open_file",
                            pid: Some(pid),
                            command: command.clone(),
                            peer_ip: None,
                            user_agent: None,
                            operation: None,
                        },
                    );
                }
            }
            _ => {}
        }
    }
    entries
        .into_iter()
        .map(|((pool, _), entry)| (pool, entry))
        .collect()
}

fn hostname() -> Option<String> {
    #[cfg(unix)]
    {
        let mut name = [0_u8; 256];
        // SAFETY: gethostname writes at most the supplied buffer length.
        if unsafe { libc::gethostname(name.as_mut_ptr().cast(), name.len()) } != 0 {
            return None;
        }
        let end = name.iter().position(|byte| *byte == 0)?;
        std::str::from_utf8(&name[..end])
            .ok()
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
    }
    #[cfg(not(unix))]
    {
        std::env::var("COMPUTERNAME").ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_ends_when_response_drops() {
        let registry = ActivityRegistry::default();
        let activity = registry.request(None, &HeaderMap::new(), "browser");
        activity.pool("events.plasmite".to_owned(), "events".to_owned());
        assert_eq!(registry.0.lock().unwrap().requests.len(), 1);
        assert_eq!(
            registry
                .0
                .lock()
                .unwrap()
                .requests
                .values()
                .next()
                .unwrap()
                .0,
            "events"
        );
        let response = activity.retain_for_response(Response::new(Body::from("response")));
        assert_eq!(registry.0.lock().unwrap().requests.len(), 1);
        drop(response);
        assert!(registry.0.lock().unwrap().requests.is_empty());
    }

    #[test]
    fn lsof_groups_handles_and_excludes_server() {
        let pools = vec![("events".to_owned(), PathBuf::from("/pools/events.plasmite"))];
        let output = b"p99\0cserver\0\nn/pools/events.plasmite\0\np42\0cpython\0\nn/pools/events.plasmite\0\nn/pools/events.plasmite\0\np43\0cfollow\0\nn/pools/events.plasmite\0\n";
        let entries = parse_lsof(output, &pools, 99);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].0, "events");
        assert_eq!(entries[0].1.pid, Some(42));
        assert_eq!(entries[0].1.command.as_deref(), Some("python"));
        assert_eq!(entries[0].1.kind, "open_file");
    }
}
