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
                        n.sup_wake(&cmd.seat)
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

    /// Post events in batches; the first batch carries `reset` and `status`.
    async fn post_events(
        &self,
        seat: &str,
        reset: bool,
        status: Option<TranscriptStatus>,
        events: Vec<amail_proto::TranscriptEvent>,
    ) -> bool {
        let mut chunks: Vec<Vec<_>> = events.chunks(BATCH).map(<[_]>::to_vec).collect();
        if chunks.is_empty() {
            chunks.push(Vec::new());
        }
        let (mut reset, mut status) = (reset, status);
        for events in chunks {
            if events.is_empty() && !reset && status.is_none() {
                continue;
            }
            let post = TranscriptPost {
                node: self.cfg.label.clone(),
                reset: std::mem::take(&mut reset),
                status: status.take(),
                events,
            };
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
                        let opened = tokio::task::spawn_blocking(move || {
                            Tail::open(std::path::Path::new(&path), harness, backfill)
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
