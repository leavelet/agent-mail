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
//!
//! Each credential with open streams has its own notification, so ending
//! the streams of one credential wakes only those streams.

use crate::auth::Auth;
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

/// A credential that can open a stream.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Cred {
    Token(i64),
    /// The hash of a browser session.
    Session(String),
}

/// The open streams of one credential.
struct Entry {
    /// Tells this entry apart from a later one for the same credential.
    id: u64,
    streams: usize,
    /// Set to `true` when the credential is revoked.
    ended: watch::Sender<bool>,
}

#[derive(Default)]
pub struct Revocations {
    open: Mutex<Open>,
}

#[derive(Default)]
struct Open {
    next_id: u64,
    creds: HashMap<Cred, Entry>,
}

/// Keeps a stream's credentials registered while it is open.
pub struct StreamGuard {
    revs: Arc<Revocations>,
    creds: Vec<(Cred, u64)>,
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        let mut open = self.revs.open.lock().unwrap();
        for (c, id) in &self.creds {
            if let Some(e) = open.creds.get_mut(c)
                && e.id == *id
            {
                e.streams -= 1;
                if e.streams == 0 {
                    open.creds.remove(c);
                }
            }
        }
    }
}

impl Revocations {
    /// Register a stream opened by `auth`. The future resolves when one of
    /// its credentials is revoked.
    pub fn open(
        self: &Arc<Self>,
        auth: &Auth,
    ) -> (StreamGuard, impl Future<Output = ()> + Send + use<>) {
        let mut creds = vec![Cred::Token(auth.token_id)];
        if let Some(s) = &auth.session {
            creds.push(Cred::Session(s.clone()));
        }
        self.open_creds(creds)
    }

    /// Register a stream opened with these credentials (see [`Self::open`]).
    pub fn open_creds(
        self: &Arc<Self>,
        creds: Vec<Cred>,
    ) -> (StreamGuard, impl Future<Output = ()> + Send + use<>) {
        let mut registered = Vec::with_capacity(creds.len());
        let mut waits = Vec::with_capacity(creds.len());
        {
            let mut open = self.open.lock().unwrap();
            let open = &mut *open;
            for c in creds {
                let e = open.creds.entry(c.clone()).or_insert_with(|| {
                    open.next_id += 1;
                    Entry {
                        id: open.next_id,
                        streams: 0,
                        ended: watch::channel(false).0,
                    }
                });
                e.streams += 1;
                registered.push((c, e.id));
                let mut rx = e.ended.subscribe();
                // Resolves when the value becomes `true`, or when the entry
                // is dropped by `end`.
                waits.push(Box::pin(async move {
                    let _ = rx.wait_for(|v| *v).await;
                }));
            }
        }
        let ended = async move {
            futures::future::select_all(waits).await;
        };
        (
            StreamGuard {
                revs: self.clone(),
                creds: registered,
            },
            ended,
        )
    }

    /// End the streams of these credentials on this instance.
    pub fn end(&self, creds: impl IntoIterator<Item = Cred>) {
        let mut open = self.open.lock().unwrap();
        for c in creds {
            if let Some(e) = open.creds.remove(&c) {
                let _ = e.ended.send(true);
            }
        }
    }

    /// The credentials of the open streams: token ids and session hashes.
    pub fn open_credentials(&self) -> (Vec<i64>, Vec<String>) {
        let open = self.open.lock().unwrap();
        let mut tokens = Vec::new();
        let mut sessions = Vec::new();
        for c in open.creds.keys() {
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

#[cfg(test)]
mod tests {
    use super::*;
    use futures::FutureExt;

    /// Ending one credential's streams ends only those, however many other
    /// credentials are ended meanwhile.
    #[tokio::test]
    async fn ends_only_the_revoked_credentials_streams() {
        let revs = Arc::new(Revocations::default());
        let (_g1, ended1) = revs.open_creds(vec![Cred::Token(1)]);
        let (_g2, ended2) = revs.open_creds(vec![Cred::Token(2), Cred::Session("s2".into())]);
        let (_g3, ended3) = revs.open_creds(vec![Cred::Token(1)]);
        let mut ended1 = Box::pin(ended1);
        let mut ended2 = Box::pin(ended2);
        let mut ended3 = Box::pin(ended3);
        // More revocations than the old broadcast channel could hold, of
        // credentials with and without open streams.
        let mut others = Vec::new();
        for i in 0..3000 {
            let (g, _) = revs.open_creds(vec![Cred::Token(100 + i)]);
            others.push(g);
        }
        revs.end((100..3100).map(Cred::Token));
        revs.end((5000..8000).map(Cred::Token));
        tokio::task::yield_now().await;
        assert!((&mut ended1).now_or_never().is_none());
        assert!((&mut ended2).now_or_never().is_none());
        assert!((&mut ended3).now_or_never().is_none());
        // The session ends stream 2 only.
        revs.end([Cred::Session("s2".into())]);
        assert!((&mut ended2).now_or_never().is_some());
        assert!((&mut ended1).now_or_never().is_none());
        // Token 1 ends both of its streams.
        revs.end([Cred::Token(1)]);
        assert!((&mut ended1).now_or_never().is_some());
        assert!((&mut ended3).now_or_never().is_some());
        // Stream 2's token stays registered until the stream is dropped.
        drop(others);
        assert_eq!(revs.open_credentials(), (vec![2], vec![]));
        drop(_g2);
        assert_eq!(revs.open_credentials(), (vec![], vec![]));
    }

    /// A stream opened with a credential after an earlier stream of it was
    /// ended is registered anew, and the old guard does not unregister it.
    #[tokio::test]
    async fn guards_of_ended_entries() {
        let revs = Arc::new(Revocations::default());
        let (g1, _) = revs.open_creds(vec![Cred::Token(1)]);
        revs.end([Cred::Token(1)]);
        let (_g2, ended2) = revs.open_creds(vec![Cred::Token(1)]);
        drop(g1);
        assert_eq!(revs.open_credentials().0, vec![1]);
        revs.end([Cred::Token(1)]);
        assert!(Box::pin(ended2).now_or_never().is_some());
    }
}
