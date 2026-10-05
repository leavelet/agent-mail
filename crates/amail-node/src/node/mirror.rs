//! The conversation mirror, node side: while the server holds a lease for a
//! seat (a `transcript` command, renewed while browsers watch), the node
//! reads the transcript file of the seat's session and posts new events.
//!
//! The file is the one the harness's hooks report (`transcript_path`); a
//! hook that reports another file (a new session) switches to it. Reading
//! follows growth, truncation and replacement of the file (see
//! [`crate::transcript::tail`]). When the lease runs out, forwarding stops.

use super::*;
use crate::transcript::tail::Tail;
use amail_proto::{NodeCommand, TranscriptPost, TranscriptState, TranscriptStatus};

/// Events per post.
const BATCH: usize = 500;
/// Bytes per post (the server refuses larger ones).
const POST_BYTES: usize = amail_proto::limits::MAX_TRANSCRIPT_POST_BYTES;

/// Split `events` into posts of at most [`BATCH`] events and
/// [`POST_BYTES`] of JSON. The first post is `first` (with its `reset` and
/// `status`); the others carry only its `node`. An event too large for a
/// post on its own has its text cut to fit.
fn batches(
    first: TranscriptPost,
    events: Vec<amail_proto::TranscriptEvent>,
) -> Vec<TranscriptPost> {
    let next = |node: &Option<String>| TranscriptPost {
        node: node.clone(),
        ..Default::default()
    };
    let node = first.node.clone();
    let mut posts = vec![first];
    let mut size = len(&posts[0]);
    for mut e in events {
        let mut n = len(&e) + 1;
        let cur = posts.last().unwrap();
        if !cur.events.is_empty() && (size + n > POST_BYTES || cur.events.len() >= BATCH) {
            posts.push(next(&node));
            size = len(posts.last().unwrap());
        }
        // A single event that does not fit: cut its text (rare: texts are
        // already cut to a few thousand characters).
        while size + n > POST_BYTES && !e.text.is_empty() {
            let keep = e.text.chars().count() / 2;
            e.text = e.text.chars().take(keep).collect();
            e.truncated = true;
            n = len(&e) + 1;
        }
        if size + n > POST_BYTES {
            e.tool = None;
            e.ts = None;
            n = len(&e) + 1;
        }
        size += n;
        posts.last_mut().unwrap().events.push(e);
    }
    posts
}

/// The length of a value's JSON.
fn len<T: serde::Serialize>(v: &T) -> usize {
    serde_json::to_vec(v).map_or(0, |v| v.len())
}

/// A forwarding task's runtime entry.
#[derive(Debug, Clone)]
pub(super) struct MirrorRt {
    pub(super) until: Instant,
    /// Send everything again (a new watch asked for a backfill).
    pub(super) resend: bool,
    pub(super) backfill: usize,
}

struct Open {
    tail: Tail,
    path: String,
    session: Option<String>,
    harness: Harness,
}

impl Node {
    /// A command from the server's event stream.
    pub(super) fn on_command(self: &Arc<Self>, cmd: NodeCommand) {
        if let (Some(want), Some(me)) = (&cmd.node, &self.cfg.label)
            && want != me
        {
            return;
        }
        match cmd.op.as_str() {
            "transcript" => self.mirror_lease(&cmd),
            "sleep" | "wake" => {
                if !self.lock().saved.managed.contains_key(&cmd.seat) {
                    return;
                }
                let n = self.clone();
                tokio::spawn(async move {
                    let r = if cmd.op == "sleep" {
                        n.sup_sleep(&cmd.seat).await
                    } else {
                        n.sup_wake(&cmd.seat).await
                    };
                    tracing::info!("server asked to {} seat {}: {r}", cmd.op, cmd.seat);
                });
            }
            other => tracing::debug!("ignoring unknown command {other:?}"),
        }
    }

    fn mirror_lease(self: &Arc<Self>, cmd: &NodeCommand) {
        let lease = Duration::from_millis(cmd.lease_ms.unwrap_or(30_000).clamp(1_000, 600_000));
        let mut st = self.lock();
        let known = st.saved.transcripts.contains_key(&cmd.seat)
            || st.saved.managed.contains_key(&cmd.seat)
            || st.seats.get(&cmd.seat).is_some_and(|s| s.harness.is_some());
        // A seat this node knows nothing about is some other node's, unless
        // the server named this node.
        if !known && cmd.node.is_none() {
            return;
        }
        // Named, but unknown here: answered with a status, within bounds.
        if !known
            && !st.mirrors.contains_key(&cmd.seat)
            && (!amail_proto::valid_name(&cmd.seat)
                || st.mirrors.len() >= amail_proto::limits::NODE_MAX_OTHER_SEATS)
        {
            return;
        }
        let until = Instant::now() + lease;
        let backfill = cmd.backfill.unwrap_or(200).min(2000) as usize;
        match st.mirrors.get_mut(&cmd.seat) {
            Some(m) => {
                m.until = m.until.max(until);
                if cmd.backfill.is_some() {
                    m.resend = true;
                    m.backfill = backfill;
                }
            }
            None => {
                st.mirrors.insert(
                    cmd.seat.clone(),
                    MirrorRt {
                        until,
                        resend: false,
                        backfill,
                    },
                );
                drop(st);
                let n = self.clone();
                let seat = cmd.seat.clone();
                tokio::spawn(async move { n.mirror_loop(seat).await });
            }
        }
    }

    async fn post_mirror(&self, seat: &str, post: TranscriptPost) -> bool {
        match self.server.post_transcript(seat, &post).await {
            Ok(_) => true,
            Err(e) => {
                tracing::debug!("posting the transcript of {seat}: {e:#}");
                false
            }
        }
    }

    async fn post_status(&self, seat: &str, status: TranscriptStatus) -> bool {
        self.post_mirror(
            seat,
            TranscriptPost {
                node: self.cfg.label.clone(),
                status: Some(status),
                ..Default::default()
            },
        )
        .await
    }

    /// Post events in batches that fit the server's post size; the first
    /// batch carries `reset` and `status`.
    async fn post_events(
        &self,
        seat: &str,
        reset: bool,
        status: Option<TranscriptStatus>,
        events: Vec<amail_proto::TranscriptEvent>,
    ) -> bool {
        let first = TranscriptPost {
            node: self.cfg.label.clone(),
            reset,
            status,
            events: Vec::new(),
        };
        for post in batches(first, events) {
            if post.events.is_empty() && !post.reset && post.status.is_none() {
                continue;
            }
            if !self.post_mirror(seat, post).await {
                return false;
            }
        }
        true
    }

    async fn mirror_loop(self: Arc<Self>, seat: String) {
        tracing::info!("mirroring the transcript of seat {seat}");
        let mut open: Option<Open> = None;
        // The last status posted, to post changes only.
        let mut shown: Option<TranscriptStatus> = None;
        loop {
            let (want, resend, backfill) = {
                let mut st = self.lock();
                let now = Instant::now();
                let Some(m) = st.mirrors.get_mut(&seat) else {
                    break;
                };
                if now >= m.until {
                    st.mirrors.remove(&seat);
                    break;
                }
                let resend = std::mem::take(&mut m.resend);
                let backfill = m.backfill;
                (st.saved.transcripts.get(&seat).cloned(), resend, backfill)
            };
            let switch = match (&open, &want) {
                (_, None) => true,
                (None, Some(_)) => true,
                (Some(o), Some(w)) => o.path != w.path,
            };
            if switch || resend {
                open = None;
                match want {
                    None => {
                        let s = TranscriptStatus {
                            message: Some(
                                "This node has not seen a session of this seat yet.".into(),
                            ),
                            ..TranscriptStatus::new(TranscriptState::NoSession)
                        };
                        if (shown.as_ref() != Some(&s) || resend)
                            && self.post_status(&seat, s.clone()).await
                        {
                            shown = Some(s);
                        }
                    }
                    Some(w) => {
                        let harness = Harness::parse(&w.harness).unwrap_or(Harness::Claude);
                        let path = w.path.clone();
                        let base = w.base.clone().unwrap_or_default();
                        let opened = tokio::task::spawn_blocking(move || {
                            let p = std::path::Path::new(&path);
                            // Checked again: links may have changed.
                            if !crate::transcript::path_under(p, std::path::Path::new(&base)) {
                                return Err(std::io::Error::new(
                                    std::io::ErrorKind::PermissionDenied,
                                    "outside the harness's transcript directory",
                                ));
                            }
                            Tail::open(p, harness, backfill)
                        })
                        .await;
                        match opened {
                            Ok(Ok((tail, events))) => {
                                let skipped = tail.stats().parse.skipped;
                                let s = TranscriptStatus {
                                    harness: Some(harness.as_str().into()),
                                    path: Some(w.path.clone()),
                                    session: w.session_id.clone(),
                                    skipped: Some(skipped),
                                    ..TranscriptStatus::new(TranscriptState::Streaming)
                                };
                                if self.post_events(&seat, true, Some(s.clone()), events).await {
                                    shown = Some(s);
                                    open = Some(Open {
                                        tail,
                                        path: w.path,
                                        session: w.session_id,
                                        harness,
                                    });
                                }
                            }
                            Ok(Err(e)) => {
                                let s = TranscriptStatus {
                                    message: Some(format!("cannot read the transcript: {e}")),
                                    harness: Some(harness.as_str().into()),
                                    path: Some(w.path.clone()),
                                    session: w.session_id.clone(),
                                    ..TranscriptStatus::new(TranscriptState::NoFile)
                                };
                                if (shown.as_ref() != Some(&s) || resend)
                                    && self.post_status(&seat, s.clone()).await
                                {
                                    shown = Some(s);
                                }
                            }
                            Err(e) => tracing::warn!("opening a transcript: {e}"),
                        }
                    }
                }
            } else if let Some(mut o) = open.take() {
                let polled = tokio::task::spawn_blocking(move || {
                    let r = o.tail.poll();
                    (o, r)
                })
                .await;
                match polled {
                    Ok((o, Ok(r))) => {
                        let skipped = o.tail.stats().parse.skipped;
                        let status = if r.missing {
                            Some(TranscriptStatus {
                                message: Some("the transcript file is gone".into()),
                                harness: Some(o.harness.as_str().into()),
                                path: Some(o.path.clone()),
                                session: o.session.clone(),
                                ..TranscriptStatus::new(TranscriptState::NoFile)
                            })
                        } else {
                            Some(TranscriptStatus {
                                harness: Some(o.harness.as_str().into()),
                                path: Some(o.path.clone()),
                                session: o.session.clone(),
                                skipped: Some(skipped),
                                ..TranscriptStatus::new(TranscriptState::Streaming)
                            })
                        }
                        .filter(|s| shown.as_ref() != Some(s));
                        if r.reset || !r.events.is_empty() || status.is_some() {
                            if self
                                .post_events(&seat, r.reset, status.clone(), r.events)
                                .await
                            {
                                if let Some(s) = status {
                                    shown = Some(s);
                                }
                                open = Some(o);
                            }
                            // On failure `open` stays empty: the next pass
                            // reopens the file and sends everything again.
                        } else {
                            open = Some(o);
                        }
                    }
                    Ok((_, Err(e))) => tracing::debug!("reading the transcript of {seat}: {e}"),
                    Err(e) => tracing::warn!("reading a transcript: {e}"),
                }
            }
            tokio::time::sleep(self.cfg.mirror_poll).await;
        }
        tracing::info!("stopped mirroring the transcript of seat {seat}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use amail_proto::{TranscriptEvent, TranscriptKind, TranscriptRole};

    fn ev(text: String) -> TranscriptEvent {
        TranscriptEvent {
            ts: Some("2026-10-04T10:00:00.000Z".into()),
            role: TranscriptRole::Assistant,
            kind: TranscriptKind::Text,
            text,
            tool: None,
            truncated: false,
            error: false,
        }
    }

    /// Large backfills are split into posts the server accepts, in order,
    /// with `reset` and `status` on the first only.
    #[test]
    fn batches_fit_the_post_size() {
        // Control characters take six bytes each in JSON.
        let events: Vec<_> = (0..300)
            .map(|i| ev(format!("{i}:{}", "\u{1}".repeat(15_990))))
            .collect();
        let first = TranscriptPost {
            node: Some("n1".into()),
            reset: true,
            status: Some(TranscriptStatus::new(TranscriptState::Streaming)),
            events: Vec::new(),
        };
        let posts = batches(first, events.clone());
        assert!(posts.len() > 20, "{} posts", posts.len());
        for (i, p) in posts.iter().enumerate() {
            assert!(len(p) <= POST_BYTES, "post {i}: {} bytes", len(p));
            assert_eq!(p.reset, i == 0);
            assert_eq!(p.status.is_some(), i == 0);
            assert_eq!(p.node.as_deref(), Some("n1"));
        }
        let all: Vec<_> = posts.into_iter().flat_map(|p| p.events).collect();
        assert_eq!(all.len(), events.len());
        assert!(all.iter().zip(&events).all(|(a, b)| a.text == b.text));

        // Many small events: at most BATCH per post.
        let small: Vec<_> = (0..1200).map(|i| ev(i.to_string())).collect();
        let posts = batches(TranscriptPost::default(), small);
        let sizes: Vec<_> = posts.iter().map(|p| p.events.len()).collect();
        assert_eq!(sizes, vec![500, 500, 200]);

        // One event too large on its own is cut to fit.
        let mut huge = ev("x".repeat(3 * POST_BYTES));
        huge.tool = Some("t".repeat(10));
        let posts = batches(TranscriptPost::default(), vec![huge]);
        assert_eq!(posts.len(), 1);
        assert!(len(&posts[0]) <= POST_BYTES);
        assert!(posts[0].events[0].truncated);
    }
}
