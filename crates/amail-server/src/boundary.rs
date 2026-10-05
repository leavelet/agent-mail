//! The boundary between orgs: what one org may see of another, and which
//! messages may cross from one to the other (see docs/concepts.md, "Other
//! orgs").
//!
//! Two things live here and nowhere else:
//!
//! - [`Reach`]: the reachability rule. Sending and replying ask it whether
//!   a message from a seat of one org may reach a seat of another, and
//!   which contact of the sender's org must be on it.
//! - [`sight`]: what org B may see of org A: the cards A opened to B (from
//!   A's grants). Cards of other orgs on every read path (the view of
//!   another org, `amail who --org`, address completion, the seats' and
//!   teams' openings) come from it; thread participants come from
//!   [`participants`].
//!
//! The rule (spec section 4): every thread that crosses two orgs has at
//! least one contact of each org on it. A contact of A for B is a seat of A
//! open to B. A message from seat `x` of A with recipients in B is accepted
//! when A and B have an active link; every recipient in B is a contact of
//! B for A or already a participant of the thread (a seat on an earlier
//! message of the thread that crossed orgs); a thread that crossed between
//! A and B still has a contact of each on it; A's `outbound` setting lets
//! `x` write outside; and the message names no list. When no contact of A
//! for B is on the message, the server adds A's primary contact for B in
//! Cc.

use crate::AppState;
use crate::error::ApiResult;
use amail_proto::{self as p, CardKind, SeatKind};
use amail_store::{CardRec, GrantFilter, GrantRec, GrantTarget, Org, Seat};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// What one org (the grantor) shows one other org (the audience): the
/// seats it opened (directly, or as contacts of an opened team) and the
/// opened teams, by name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpenSet {
    pub org_id: i64,
    pub org_name: String,
    pub seats: BTreeMap<String, CardRec>,
    pub teams: BTreeMap<String, CardRec>,
    /// Seat names in the order their grants were made (the first one is
    /// the default primary contact after the user seat).
    pub order: Vec<String>,
}

impl OpenSet {
    pub fn has_seat_id(&self, id: i64) -> bool {
        self.seats.values().any(|c| c.id == id)
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
                if set.seats.insert(s.name.clone(), s.clone()).is_none() {
                    set.order.push(s.name.clone());
                }
            }
            GrantTarget::Team { team, contacts } => {
                set.teams.insert(team.name.clone(), team.clone());
                for s in contacts {
                    if set.seats.insert(s.name.clone(), s.clone()).is_none() {
                        set.order.push(s.name.clone());
                    }
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

/// Run `change` (something that may change what `org` shows other orgs)
/// and return its result with the changes of sight.
pub async fn changing<T>(
    st: &AppState,
    org: &Org,
    change: impl std::future::Future<Output = ApiResult<T>>,
) -> ApiResult<(T, Vec<p::SightChange>)> {
    let before = shown_by(st, org).await?;
    let out = change.await?;
    let after = shown_by(st, org).await?;
    Ok((out, diff(&before, &after)))
}

/// Why a recipient of another org is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Accepted,
    /// Not a contact for the sender's org, not a participant of the
    /// thread, the sender may not write outside, or no such seat: answered
    /// alike.
    Unreachable,
    /// The thread crossed between the two orgs before and one of them has
    /// no contact left on it.
    ThreadClosed,
}

/// The reachability rule (see the module documentation), with the facts it
/// needs looked up once per send.
pub struct Reach<'a> {
    st: &'a AppState,
    org: &'a Org,
    sender: &'a Seat,
    thread: Option<i64>,
    links: HashMap<i64, bool>,
    /// Per other org: its contacts for the sender's org.
    theirs: HashMap<i64, OpenSet>,
    /// Per other org: the sender's org's contacts for it.
    ours: HashMap<i64, OpenSet>,
    /// The thread's participants before this message: seat id -> org id.
    parts: Option<HashMap<i64, i64>>,
}

impl<'a> Reach<'a> {
    pub fn new(st: &'a AppState, org: &'a Org, sender: &'a Seat, thread: Option<i64>) -> Self {
        Reach {
            st,
            org,
            sender,
            thread,
            links: HashMap::new(),
            theirs: HashMap::new(),
            ours: HashMap::new(),
            parts: None,
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

    /// The contacts of `to` for the sender's org.
    pub async fn theirs(&mut self, to: &Org) -> ApiResult<&OpenSet> {
        if !self.theirs.contains_key(&to.id) {
            let set = sight(self.st, self.org, Some(to))
                .await?
                .remove(&to.name)
                .unwrap_or_default();
            self.theirs.insert(to.id, set);
        }
        Ok(&self.theirs[&to.id])
    }

    /// The sender's org's contacts for `to`.
    pub async fn ours(&mut self, to: &Org) -> ApiResult<&OpenSet> {
        if !self.ours.contains_key(&to.id) {
            let set = sight(self.st, to, Some(self.org))
                .await?
                .remove(&self.org.name)
                .unwrap_or_default();
            self.ours.insert(to.id, set);
        }
        Ok(&self.ours[&to.id])
    }

    /// The thread's participants before this message (seat id -> org id).
    pub async fn parts(&mut self) -> ApiResult<&HashMap<i64, i64>> {
        if self.parts.is_none() {
            let map = match self.thread {
                Some(t) => self
                    .st
                    .store
                    .thread_parts(&[t])
                    .await?
                    .into_iter()
                    .map(|p| (p.card.id, p.org_id))
                    .collect(),
                None => HashMap::new(),
            };
            self.parts = Some(map);
        }
        Ok(self.parts.as_ref().expect("loaded"))
    }

    /// Whether the org's `outbound` setting lets the sender write to `to`.
    async fn may_write_out(&mut self, to: &Org) -> ApiResult<bool> {
        if self.org.outbound == p::Outbound::AnySeat || self.sender.kind == SeatKind::User {
            return Ok(true);
        }
        let id = self.sender.id;
        Ok(self.ours(to).await?.has_seat_id(id))
    }

    /// Whether the thread crossed between the two orgs before and has lost
    /// every contact of one of them.
    async fn thread_closed(&mut self, to: &Org) -> ApiResult<bool> {
        let parts: Vec<(i64, i64)> = self.parts().await?.iter().map(|(s, o)| (*s, *o)).collect();
        if !parts.iter().any(|(_, o)| *o == to.id) {
            return Ok(false);
        }
        let theirs = self.theirs(to).await?.clone();
        let ours = self.ours(to).await?.clone();
        let has =
            |set: &OpenSet, org: i64| parts.iter().any(|(s, o)| *o == org && set.has_seat_id(*s));
        Ok(!(has(&theirs, to.id) && has(&ours, self.org.id)))
    }

    /// The rule for one recipient, seat `seat` (`None`: no seat of that
    /// name) of the other org `to`, which has an active link with the
    /// sender's org. The same facts are looked up whether or not the seat
    /// exists, so that the answer takes the same time either way.
    pub async fn check(&mut self, to: &Org, seat: Option<&Seat>) -> ApiResult<Verdict> {
        let id = seat.map_or(0, |s| s.id);
        let contact = self.theirs(to).await?.has_seat_id(id);
        let participant = self.parts().await?.get(&id) == Some(&to.id);
        let out = self.may_write_out(to).await?;
        let closed = self.thread_closed(to).await?;
        Ok(if seat.is_none() || !decide(contact, participant, out) {
            Verdict::Unreachable
        } else if closed {
            Verdict::ThreadClosed
        } else {
            Verdict::Accepted
        })
    }

    /// The seat of the sender's org to add in Cc for `to` when no contact of
    /// the sender's org is on the message: the primary contact of the
    /// sender's team (or of a team above it), else the org's, while it is
    /// still a contact for `to`; else the user seat if it is open to `to`;
    /// else the seat opened to `to` first. `None`: the org has no contact
    /// for `to`.
    pub async fn primary(&mut self, to: &Org) -> ApiResult<Option<CardRec>> {
        let ours = self.ours(to).await?.clone();
        if ours.seats.is_empty() {
            return Ok(None);
        }
        let set = self.st.store.primaries(self.org.id).await?;
        let by_id = |id: i64| ours.seats.values().find(|c| c.id == id).cloned();
        // The sender's team, then the teams above it.
        if let Some(mut team) = self.sender.team_id {
            let teams = self.st.store.teams(self.org.id).await?;
            for _ in 0..teams.len() {
                if let Some(c) = set
                    .iter()
                    .find(|(p, t, _)| *p == to.id && *t == Some(team))
                    .and_then(|(_, _, s)| by_id(*s))
                {
                    return Ok(Some(c));
                }
                match teams
                    .iter()
                    .find(|t| t.id == team)
                    .and_then(|t| t.parent_id)
                {
                    Some(up) => team = up,
                    None => break,
                }
            }
        }
        if let Some(c) = set
            .iter()
            .find(|(p, t, _)| *p == to.id && t.is_none())
            .and_then(|(_, _, s)| by_id(*s))
        {
            return Ok(Some(c));
        }
        if let Some(u) = ours.seats.values().find(|c| c.kind == Some(SeatKind::User)) {
            return Ok(Some(u.clone()));
        }
        Ok(ours.order.first().and_then(|n| ours.seats.get(n)).cloned())
    }
}

/// Section 4 of the spec for one recipient, from its facts: a contact for
/// the sender's org or a participant of the thread, and the sender may
/// write outside. (The link and the thread's contacts are checked before.)
pub fn decide(contact: bool, participant: bool, outbound: bool) -> bool {
    (contact || participant) && outbound
}

/// The org's primary contact for `peer` as shown to the org (no sender
/// team): the org's own if still a contact, else the default.
pub async fn org_primary(st: &AppState, org: &Org, peer: &Org) -> ApiResult<Option<String>> {
    let user = st.store.user_seat(org.id).await?;
    let Some(user) = user else { return Ok(None) };
    let mut plain = user.clone();
    plain.team_id = None;
    let mut r = Reach::new(st, org, &plain, None);
    Ok(r.primary(peer).await?.map(|c| c.name))
}

/// Cards of the participants of `thread` that belong to other orgs than
/// `viewer`'s.
pub async fn participants(st: &AppState, viewer: &Org, thread: i64) -> ApiResult<Vec<p::Card>> {
    Ok(st
        .store
        .thread_parts(&[thread])
        .await?
        .iter()
        .filter(|p| p.org_id != viewer.id)
        .map(|p| card(&p.card, &p.org_name))
        .collect())
}
