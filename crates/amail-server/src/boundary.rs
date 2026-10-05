//! The boundary between orgs: what one org may see of another, and which
//! messages may cross from one to the other (see docs/concepts.md,
//! "Openings").
//!
//! Two things live here and nowhere else:
//!
//! - [`Reach`]: the reachability rule. Sending, replying and list
//!   expansion ask it whether a message from a seat of one org may reach a
//!   seat of another; list displays ask it whether an external member
//!   would get mail.
//! - [`sight`] and [`View`]: what org B may see of org A. [`sight`] gives
//!   the cards A opened to B (from A's grants); [`View`] adds the thread
//!   participants and renders every message header, card and delivery row
//!   that B is shown. Every read path that shows another org's seats goes
//!   through them.
//!
//! The rule (spec section 4): a message from seat `x` of org A to seat `y`
//! of org B is accepted when A and B have an active link, `y` is open to A
//! (a grant on `y`, or on a team of which `y` is a contact, whose audience
//! is every linked org or A) or `y` wrote a message of this thread that
//! reached a seat of A, and A lets `x` write outside (A's `outbound` is
//! `any-seat`, or `x` is A's user, or `x` is open to B).

use crate::AppState;
use crate::error::ApiResult;
use crate::render;
use amail_proto::{self as p, Address, CardKind, SeatKind};
use amail_store::{CardRec, GrantFilter, GrantRec, GrantTarget, MessageRec, Org, Seat};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// What one org (the grantor) shows one other org (the audience): the
/// seats it opened (directly, or as contacts of an opened team) and the
/// opened teams, by name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpenSet {
    pub org_id: i64,
    pub org_name: String,
    pub seats: BTreeMap<String, CardRec>,
    pub teams: BTreeMap<String, CardRec>,
}

impl OpenSet {
    pub fn has_seat(&self, name: &str) -> bool {
        self.seats.contains_key(name)
    }

    /// The cards, teams first, as the audience sees them (`name@org`).
    pub fn cards(&self) -> Vec<p::Card> {
        self.teams
            .values()
            .chain(self.seats.values())
            .map(|c| card(c, &self.org_name))
            .collect()
    }

    /// Names of the cards (`name` for seats, `name (team)` for teams).
    fn names(&self) -> BTreeSet<String> {
        self.seats
            .keys()
            .cloned()
            .chain(self.teams.keys().map(|t| format!("{t} (team)")))
            .collect()
    }
}

/// A card as shown to another org.
pub fn card(c: &CardRec, org: &str) -> p::Card {
    p::Card {
        address: format!("{}@{org}", c.name),
        kind: card_kind(c),
        description: c
            .description
            .as_deref()
            .map(|d| amail_proto::text::line(d).into_owned()),
    }
}

pub fn card_kind(c: &CardRec) -> CardKind {
    match c.kind {
        None => CardKind::Team,
        Some(SeatKind::User) => CardKind::User,
        Some(_) => CardKind::Agent,
    }
}

/// The openings of section 3: which cards each grantor of `grants` shows
/// the org named `audience`. A grant counts when its audience is every
/// linked org or that org; a team grant shows the team and opens its
/// contact seats. Whether the two orgs are linked is the caller's
/// question (the store's audience filter asks it).
pub fn opened(grants: &[GrantRec], audience: &str) -> HashMap<i64, OpenSet> {
    let mut out: HashMap<i64, OpenSet> = HashMap::new();
    for g in grants {
        if g.audience.as_deref().is_some_and(|a| a != audience) {
            continue;
        }
        let set = out.entry(g.org_id).or_insert_with(|| OpenSet {
            org_id: g.org_id,
            org_name: g.org_name.clone(),
            ..Default::default()
        });
        match &g.target {
            GrantTarget::Seat(s) => {
                set.seats.insert(s.name.clone(), s.clone());
            }
            GrantTarget::Team { team, contacts } => {
                set.teams.insert(team.name.clone(), team.clone());
                for s in contacts {
                    set.seats.insert(s.name.clone(), s.clone());
                }
            }
        }
    }
    out
}

/// What `viewer` may see of the orgs it has an active link with (or of
/// `only`): the cards each of them opened to it, by org name. Thread
/// participants are not part of it (see [`View`]).
pub async fn sight(
    st: &AppState,
    viewer: &Org,
    only: Option<&Org>,
) -> ApiResult<HashMap<String, OpenSet>> {
    let grants = st
        .store
        .grants(&GrantFilter {
            grantor: only.map(|o| o.id),
            audience: Some((viewer.id, viewer.name.clone())),
        })
        .await?;
    Ok(opened(&grants, &viewer.name)
        .into_values()
        .map(|s| (s.org_name.clone(), s))
        .collect())
}

/// What `org` shows each org it has an active link with, by audience org
/// name (all of them, also those it shows nothing). Built from all of its
/// grants with [`opened`], so that a change can be compared before and
/// after.
pub async fn shown_by(st: &AppState, org: &Org) -> ApiResult<BTreeMap<String, OpenSet>> {
    let grants = st
        .store
        .grants(&GrantFilter {
            grantor: Some(org.id),
            audience: None,
        })
        .await?;
    let links = st.store.links(org.id).await?;
    Ok(shown_with(org, &grants, &links))
}

/// [`shown_by`] from loaded grants and links (also for previews).
pub fn shown_with(
    org: &Org,
    grants: &[GrantRec],
    links: &[amail_store::Link],
) -> BTreeMap<String, OpenSet> {
    links
        .iter()
        .filter(|l| l.state == p::LinkState::Active)
        .map(|l| {
            let set = opened(grants, &l.peer_name)
                .remove(&org.id)
                .unwrap_or_else(|| OpenSet {
                    org_id: org.id,
                    org_name: org.name.clone(),
                    ..Default::default()
                });
            (l.peer_name.clone(), set)
        })
        .collect()
}

/// What changed between two [`shown_by`] results, per audience org.
pub fn diff(
    before: &BTreeMap<String, OpenSet>,
    after: &BTreeMap<String, OpenSet>,
) -> Vec<p::SightChange> {
    let empty = OpenSet::default();
    let orgs: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    let mut out = Vec::new();
    for org in orgs {
        let b = before.get(org).unwrap_or(&empty).names();
        let a = after.get(org).unwrap_or(&empty).names();
        let gains: Vec<String> = a.difference(&b).cloned().collect();
        let loses: Vec<String> = b.difference(&a).cloned().collect();
        if !gains.is_empty() || !loses.is_empty() {
            out.push(p::SightChange {
                org: org.clone(),
                gains,
                loses,
            });
        }
    }
    out
}

/// After a change of what `org` shows: the seats an audience org no longer
/// sees stop being thread participants for it, so that the closing holds
/// in existing threads too.
pub async fn close_lost(
    st: &AppState,
    before: &BTreeMap<String, OpenSet>,
    after: &BTreeMap<String, OpenSet>,
) -> ApiResult<()> {
    for (org, b) in before {
        let lost: Vec<i64> = b
            .seats
            .iter()
            .filter(|(n, _)| after.get(org).is_none_or(|a| !a.seats.contains_key(*n)))
            .map(|(_, c)| c.id)
            .collect();
        if lost.is_empty() {
            continue;
        }
        if let Some(o) = st.store.org_by_name(org).await? {
            st.store.forget_thread_peers(o.id, &lost).await?;
        }
    }
    Ok(())
}

/// Run `change` (something that may change what `org` shows other orgs)
/// and return its result with the changes of sight, after closing what
/// was lost (see [`close_lost`]).
pub async fn changing<T>(
    st: &AppState,
    org: &Org,
    change: impl std::future::Future<Output = ApiResult<T>>,
) -> ApiResult<(T, Vec<p::SightChange>)> {
    let before = shown_by(st, org).await?;
    let out = change.await?;
    let after = shown_by(st, org).await?;
    close_lost(st, &before, &after).await?;
    Ok((out, diff(&before, &after)))
}

/// The reachability rule (see the module documentation), with the facts it
/// needs looked up once per send.
pub struct Reach<'a> {
    st: &'a AppState,
    org: &'a Org,
    /// `None`: no particular sender (whether a list member would get mail).
    sender: Option<&'a Seat>,
    thread: Option<i64>,
    links: HashMap<i64, bool>,
    /// Per other org: what it opened to the sender's org.
    open: HashMap<i64, OpenSet>,
    /// Per other org: whether the sender may write to it.
    outbound: HashMap<i64, bool>,
    /// Seats of other orgs that wrote into the thread, in mail that reached
    /// the sender's org.
    peers: Option<HashSet<i64>>,
}

impl<'a> Reach<'a> {
    pub fn new(
        st: &'a AppState,
        org: &'a Org,
        sender: Option<&'a Seat>,
        thread: Option<i64>,
    ) -> Self {
        Reach {
            st,
            org,
            sender,
            thread,
            links: HashMap::new(),
            open: HashMap::new(),
            outbound: HashMap::new(),
            peers: None,
        }
    }

    /// Whether the sender's org has an active link with `org_id` (its own
    /// org: yes).
    pub async fn linked(&mut self, org_id: i64) -> ApiResult<bool> {
        if org_id == self.org.id {
            return Ok(true);
        }
        if let Some(&b) = self.links.get(&org_id) {
            return Ok(b);
        }
        let b = self.st.store.link_active(self.org.id, org_id).await?;
        self.links.insert(org_id, b);
        Ok(b)
    }

    async fn open_set(&mut self, to: &Org) -> ApiResult<&OpenSet> {
        if !self.open.contains_key(&to.id) {
            let set = sight(self.st, self.org, Some(to))
                .await?
                .remove(&to.name)
                .unwrap_or_default();
            self.open.insert(to.id, set);
        }
        Ok(&self.open[&to.id])
    }

    async fn peers(&mut self) -> ApiResult<&HashSet<i64>> {
        if self.peers.is_none() {
            let set = match self.thread {
                Some(t) => self
                    .st
                    .store
                    .thread_peers(self.org.id, &[t])
                    .await?
                    .into_iter()
                    .map(|p| p.card.id)
                    .collect(),
                None => HashSet::new(),
            };
            self.peers = Some(set);
        }
        Ok(self.peers.as_ref().expect("loaded"))
    }

    /// Whether the org's `outbound` setting lets the sender write to `to`.
    async fn may_write_out(&mut self, to: &Org) -> ApiResult<bool> {
        let Some(sender) = self.sender else {
            return Ok(true);
        };
        if self.org.outbound == p::Outbound::AnySeat || sender.kind == SeatKind::User {
            return Ok(true);
        }
        if let Some(&b) = self.outbound.get(&to.id) {
            return Ok(b);
        }
        // What the sender's org shows `to`, as `to` sees it.
        let b = sight(self.st, to, Some(self.org))
            .await?
            .get(&self.org.name)
            .is_some_and(|s| s.has_seat(&sender.name));
        self.outbound.insert(to.id, b);
        Ok(b)
    }

    /// Whether a message may reach seat `seat` (`None`: there is no seat of
    /// that name) of `to`. The same facts are looked up whether or not the
    /// seat exists, so that the answer takes the same time either way.
    pub async fn allows(&mut self, to: &Org, seat: Option<&Seat>) -> ApiResult<bool> {
        if to.id == self.org.id {
            return Ok(seat.is_some());
        }
        let linked = self.linked(to.id).await?;
        let open = self
            .open_set(to)
            .await?
            .has_seat(seat.map_or("", |s| &s.name));
        let participant = self.peers().await?.contains(&seat.map_or(0, |s| s.id));
        let out = self.may_write_out(to).await?;
        Ok(seat.is_some() && decide(linked, open, participant, out))
    }
}

/// Section 4 of the spec, from its facts.
pub fn decide(linked: bool, open: bool, participant: bool, outbound: bool) -> bool {
    linked && (open || participant) && outbound
}

/// What `viewer` is shown of messages and deliveries: the cards other orgs
/// opened to it and the participants of the threads in question. Load it
/// with [`View::load`] for the messages to be shown; it looks nothing up
/// when they name no seat of another org.
pub struct View {
    pub viewer: Org,
    open: HashMap<String, OpenSet>,
    /// Thread -> `(org name, seat name)` of its participants of other orgs.
    peers: HashMap<i64, HashSet<(String, String)>>,
}

impl View {
    /// A view that shows nothing of other orgs (for messages that name
    /// none).
    pub fn own(viewer: &Org) -> View {
        View {
            viewer: viewer.clone(),
            open: HashMap::new(),
            peers: HashMap::new(),
        }
    }

    /// The view for showing `msgs` to `viewer`.
    pub async fn load<'m>(
        st: &AppState,
        viewer: &Org,
        msgs: impl IntoIterator<Item = &'m MessageRec>,
    ) -> ApiResult<View> {
        let mut v = View::own(viewer);
        let threads: BTreeSet<i64> = msgs
            .into_iter()
            .filter(|m| m.org_id != viewer.id && v.names_others(m))
            .map(|m| m.thread_id)
            .collect();
        v.fill(st, threads).await?;
        Ok(v)
    }

    /// The view for showing deliveries of messages of these threads (for
    /// example a message's delivery status).
    pub async fn for_threads(st: &AppState, viewer: &Org, threads: &[i64]) -> ApiResult<View> {
        let mut v = View::own(viewer);
        v.fill(st, threads.iter().copied().collect()).await?;
        Ok(v)
    }

    async fn fill(&mut self, st: &AppState, threads: BTreeSet<i64>) -> ApiResult<()> {
        if threads.is_empty() {
            return Ok(());
        }
        self.open = sight(st, &self.viewer, None).await?;
        let ids: Vec<i64> = threads.into_iter().collect();
        for p in st.store.thread_peers(self.viewer.id, &ids).await? {
            self.peers
                .entry(p.thread_id)
                .or_default()
                .insert((p.org_name, p.card.name));
        }
        Ok(())
    }

    /// Whether `m` names a seat or list of an org other than the viewer's.
    fn names_others(&self, m: &MessageRec) -> bool {
        let own = format!("@{}", self.viewer.name);
        m.to_addrs
            .iter()
            .chain(&m.cc_addrs)
            .any(|a| !a.ends_with(&own))
    }

    /// Whether the viewer may see seat `name` of org `org` in `thread`: its
    /// own seats; another org's seats that are open to it, or that wrote
    /// into the thread in mail that reached it.
    pub fn shows(&self, thread: Option<i64>, org: &str, name: &str) -> bool {
        org == self.viewer.name
            || self.open.get(org).is_some_and(|s| s.has_seat(name))
            || thread.is_some_and(|t| {
                self.peers
                    .get(&t)
                    .is_some_and(|p| p.contains(&(org.to_string(), name.to_string())))
            })
    }

    /// Addresses of `list` the viewer may see (relative to its org), and
    /// how many it may not.
    fn mask(&self, m: &MessageRec, list: &[String]) -> (Vec<String>, u32) {
        let mut shown = Vec::with_capacity(list.len());
        let mut hidden = 0;
        for a in list {
            let visible = m.org_id == self.viewer.id
                || match a.parse::<Address>() {
                    Ok(addr) => {
                        let org = addr.org.as_deref().unwrap_or(&m.org_name);
                        self.shows(Some(m.thread_id), org, &addr.name)
                    }
                    Err(_) => false,
                };
            if visible {
                shown.push(render::rel(a, &self.viewer));
            } else {
                hidden += 1;
            }
        }
        (shown, hidden)
    }

    /// The header of `m` as the viewer sees it. The viewer's org's own
    /// messages are shown as written; in the messages of other orgs, the
    /// addresses of other orgs' seats and lists that the viewer may not see
    /// are counted instead of named.
    pub fn header(&self, m: &MessageRec) -> p::MessageHeader {
        let (to, to_hidden) = self.mask(m, &m.to_addrs);
        let (cc, cc_hidden) = self.mask(m, &m.cc_addrs);
        p::MessageHeader {
            id: m.id,
            thread_id: m.thread_id,
            in_reply_to: m.parent_id,
            from: render::addr(&m.sender_name, &m.org_name, &self.viewer),
            source: render::source(m, &self.viewer),
            to,
            cc,
            subject: m.subject.clone(),
            priority: m.priority,
            ack_required: m.ack_required,
            lines: m.lines,
            created_at: render::ts(m.created_at),
            meta: m.meta.clone(),
            expired: m.expired_at.is_some(),
            to_hidden,
            cc_hidden,
        }
    }

    pub fn message(&self, m: &MessageRec) -> p::Message {
        p::Message {
            header: self.header(m),
            body: m.body.clone(),
            body_omitted: false,
        }
    }

    pub fn entry(&self, e: &amail_store::Entry) -> p::InboxEntry {
        p::InboxEntry {
            delivery: render::delivery(&e.delivery, &self.viewer),
            message: self.header(&e.message),
            body: None,
            preview: None,
        }
    }

    /// Whether a delivery of a message of `thread` to `d`'s seat may be
    /// shown to the viewer (its own seats, and other orgs' seats it may
    /// see).
    pub fn shows_delivery(&self, thread: i64, d: &amail_store::DeliveryRec) -> bool {
        d.org_id == self.viewer.id || self.shows(Some(thread), &d.org_name, &d.seat_name)
    }

    /// Cards of the participants of other orgs in `thread`.
    pub async fn participants(st: &AppState, viewer: &Org, thread: i64) -> ApiResult<Vec<p::Card>> {
        Ok(st
            .store
            .thread_peers(viewer.id, &[thread])
            .await?
            .iter()
            .map(|p| card(&p.card, &p.org_name))
            .collect())
    }
}
