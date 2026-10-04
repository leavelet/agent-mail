//! Ending open streams when the credential that opened them stops being
//! valid.
//!
//! Event streams and conversation mirrors are authenticated once, when they
//! open. Each of them registers its token (and browser session, if it was
//! opened with the session cookie) here and ends when that credential is
//! revoked:
//!
//! - at once on this instance, when the revocation happens here (token
//!   revocation and rotation, signing out);
//! - on the other instances through the cluster notification;
//! - in every case at the next sweep (see `public::sweep`), which checks
//!   the credentials of all open streams against the database. This covers
//!   revocations made directly on the database (`amail admin token reset`),
//!   sessions ended because a token has too many, and expired sessions.

use crate::auth::Auth;
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

/// A credential that can open a stream.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Cred {
    Token(i64),
    /// The hash of a browser session.
    Session(String),
}

pub struct Revocations {
    tx: broadcast::Sender<Cred>,
    open: Mutex<HashMap<Cred, usize>>,
}

/// Keeps a stream's credentials registered while it is open.
pub struct StreamGuard {
    revs: Arc<Revocations>,
    creds: Vec<Cred>,
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        let mut open = self.revs.open.lock().unwrap();
        for c in &self.creds {
            if let Some(n) = open.get_mut(c) {
                *n -= 1;
                if *n == 0 {
                    open.remove(c);
                }
            }
        }
    }
}

impl Default for Revocations {
    fn default() -> Self {
        Revocations {
            tx: broadcast::channel(1024).0,
            open: Mutex::new(HashMap::new()),
        }
    }
}

impl Revocations {
    /// Register a stream opened by `auth`. The future resolves when one of
    /// its credentials is revoked (or when revocations were missed, so that
    /// the client reconnects and authenticates again).
    pub fn open(
        self: &Arc<Self>,
        auth: &Auth,
    ) -> (StreamGuard, impl Future<Output = ()> + Send + use<>) {
        let mut creds = vec![Cred::Token(auth.token_id)];
        if let Some(s) = &auth.session {
            creds.push(Cred::Session(s.clone()));
        }
        let mut rx = self.tx.subscribe();
        {
            let mut open = self.open.lock().unwrap();
            for c in &creds {
                *open.entry(c.clone()).or_default() += 1;
            }
        }
        let watched = creds.clone();
        let ended = async move {
            loop {
                match rx.recv().await {
                    Ok(c) if watched.contains(&c) => return,
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(_)) => return,
                    Err(broadcast::error::RecvError::Closed) => std::future::pending().await,
                }
            }
        };
        (
            StreamGuard {
                revs: self.clone(),
                creds,
            },
            ended,
        )
    }

    /// End the streams of these credentials on this instance.
    pub fn end(&self, creds: impl IntoIterator<Item = Cred>) {
        let open = self.open.lock().unwrap();
        for c in creds {
            if open.contains_key(&c) {
                let _ = self.tx.send(c);
            }
        }
    }

    /// The credentials of the open streams: token ids and session hashes.
    pub fn open_credentials(&self) -> (Vec<i64>, Vec<String>) {
        let open = self.open.lock().unwrap();
        let mut tokens = Vec::new();
        let mut sessions = Vec::new();
        for c in open.keys() {
            match c {
                Cred::Token(id) => tokens.push(*id),
                Cred::Session(h) => sessions.push(h.clone()),
            }
        }
        (tokens, sessions)
    }

    /// End the streams whose credentials are no longer live.
    pub async fn sweep(&self, store: &dyn amail_store::Store) -> amail_store::Result<()> {
        let (tokens, sessions) = self.open_credentials();
        if tokens.is_empty() && sessions.is_empty() {
            return Ok(());
        }
        let (live_t, live_s) = store.live_credentials(&tokens, &sessions).await?;
        let dead = tokens
            .into_iter()
            .filter(|t| !live_t.contains(t))
            .map(Cred::Token)
            .chain(
                sessions
                    .into_iter()
                    .filter(|s| !live_s.contains(s))
                    .map(Cred::Session),
            );
        self.end(dead);
        Ok(())
    }
}
