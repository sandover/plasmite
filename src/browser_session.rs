//! Browser sessions persist without storing access keys or raw cookie values.

use crate::access_store::{AccessGrant, AccessStore, ensure_private, read_json, write_atomic_json};
use getrandom::fill;
use plasmite::api::{Error, ErrorKind};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(super) const COOKIE_NAME: &str = "plasmite_session";
pub(super) const LIFETIME: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const MAX_SESSIONS: usize = 1024;

pub(super) struct BrowserSessions {
    access: Option<Arc<AccessStore>>,
    path: Option<PathBuf>,
    sessions: Mutex<HashMap<String, Session>>,
}

#[derive(Deserialize, Serialize)]
struct Session {
    key_id: String,
    expires_at: u64,
}

impl BrowserSessions {
    pub(super) fn new(access: Option<Arc<AccessStore>>) -> Result<Self, Error> {
        let path = access
            .as_ref()
            .map(|store| store.state_dir().join("browser-sessions.json"));
        let sessions = if let Some(path) = &path
            && path.exists()
        {
            ensure_private(path)?;
            read_json(path)?
        } else {
            HashMap::new()
        };
        Ok(Self {
            access,
            path,
            sessions: Mutex::new(sessions),
        })
    }

    pub(super) fn create(&self, key_id: &str) -> Result<String, Error> {
        let mut random = [0u8; 32];
        fill(&mut random).map_err(|err| {
            Error::new(ErrorKind::Internal)
                .with_message(format!("failed to create browser session: {err}"))
        })?;
        let token: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let now = unix_seconds()?;
        let mut sessions = self.sessions.lock().map_err(|_| session_unavailable())?;
        sessions.retain(|_, session| {
            session.expires_at > now
                && self
                    .access
                    .as_ref()
                    .is_some_and(|access| access.authorize_id(&session.key_id).is_some())
        });
        if sessions.len() >= MAX_SESSIONS {
            return Err(Error::new(ErrorKind::Busy)
                .with_message("too many active browser sessions")
                .with_hint("Sign out of an unused browser session or ask the owner to revoke an unused access key."));
        }
        sessions.insert(
            token_hash(&token),
            Session {
                key_id: key_id.to_owned(),
                expires_at: now + LIFETIME.as_secs(),
            },
        );
        if let Err(err) = self.persist(&sessions) {
            self.rejoin(&mut sessions);
            return Err(err);
        }
        Ok(token)
    }

    pub(super) fn grant(&self, token: &str) -> Option<AccessGrant> {
        let now = unix_seconds().ok()?;
        let sessions = self.sessions.lock().ok()?;
        let session = sessions.get(&token_hash(token))?;
        if session.expires_at <= now {
            return None;
        }
        self.access.as_ref()?.authorize_id(&session.key_id)
    }

    pub(super) fn remove(&self, token: &str) -> Result<(), Error> {
        let mut sessions = self.sessions.lock().map_err(|_| session_unavailable())?;
        let key = token_hash(token);
        let Some(_) = sessions.remove(&key) else {
            return Ok(());
        };
        if let Err(err) = self.persist(&sessions) {
            self.rejoin(&mut sessions);
            return Err(err);
        }
        Ok(())
    }

    fn persist(&self, sessions: &HashMap<String, Session>) -> Result<(), Error> {
        let path = self.path.as_ref().ok_or_else(session_unavailable)?;
        write_atomic_json(path, sessions)
    }

    fn rejoin(&self, sessions: &mut HashMap<String, Session>) {
        *sessions = self
            .path
            .as_ref()
            .and_then(|path| read_json(path).ok())
            .unwrap_or_default();
    }
}

fn token_hash(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn unix_seconds() -> Result<u64, Error> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|time| time.as_secs())
        .map_err(|_| {
            Error::new(ErrorKind::Internal).with_message("system clock is before Unix epoch")
        })
}

fn session_unavailable() -> Error {
    Error::new(ErrorKind::Internal).with_message("browser sessions are unavailable")
}
