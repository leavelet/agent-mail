//! Teams, grants (openings) and the view of another org: `/v1/teams`,
//! `/v1/grants`, `/v1/orgs/{name}`. Reads need a user or seat token;
//! changes need the user token (agents cannot open or close anything).

use crate::AppState;
use crate::auth::Auth;
use crate::boundary::{self, OpenSet};
use crate::error::{ApiError, ApiResult, Json, Path};
use crate::render;
use amail_proto::{self as p, LinkState, OrgVisibility, valid_name};
use amail_store::{CardRec, GrantFilter, GrantRec, GrantTarget, Org, TeamChange};
use axum::extract::State;
use axum::http::StatusCode;
use std::collections::HashMap;

fn check_name(what: &str, name: &str) -> ApiResult<()> {
    if valid_name(name) {
        Ok(())
    } else {
        Err(ApiError::bad_request(format!(
            "invalid {what} name {name:?}: must match [a-z0-9][a-z0-9._-]{{0,62}}"
        )))
    }
}

/// The org's grants (all of them).
pub async fn own_grants(st: &AppState, org: &Org) -> ApiResult<Vec<GrantRec>> {
    Ok(st
        .store
        .grants(&GrantFilter {
            grantor: Some(org.id),
            audience: None,
        })
        .await?)
}

/// Grants as shown to the org: `active` when the audience is every linked
/// org, or a named org with an active link.
async fn grants_out(st: &AppState, org: &Org, grants: &[GrantRec]) -> ApiResult<Vec<p::Grant>> {
    let links = st.store.links(org.id).await?;
    let active = |a: &Option<String>| match a {
        None => true,
        Some(n) => links
            .iter()
            .any(|l| &l.peer_name == n && l.state == LinkState::Active),
    };
    Ok(grants
        .iter()
        .map(|g| render::grant(g, active(&g.audience)))
        .collect())
}

/// Lines describing changes of sight, for notices and answers.
pub fn changes_text(changes: &[p::SightChange]) -> String {
    if changes.is_empty() {
        return "No other org sees anything different.\n".to_string();
    }
    let mut out = String::new();
    for c in changes {
        if !c.gains.is_empty() {
            out.push_str(&format!("- {} now sees: {}\n", c.org, c.gains.join(", ")));
        }
        if !c.loses.is_empty() {
            out.push_str(&format!(
                "- {} no longer sees: {}\n",
                c.org,
                c.loses.join(", ")
            ));
        }
    }
    out
}

/// What the org shows other orgs now, as text for a notice.
async fn openings_text(st: &AppState, org: &Org) -> ApiResult<String> {
    let grants = own_grants(st, org).await?;
    let out = grants_out(st, org, &grants).await?;
    if out.is_empty() {
        return Ok("Your org opens nothing to other orgs.\n".into());
    }
    let mut s = String::from("Your openings now:\n");
    for g in out {
        s.push_str(&format!(
            "- {} ({}) to {}{}\n",
            g.target,
            g.kind,
            g.audience,
            if g.active { "" } else { " (waits for a link)" }
        ));
    }
    Ok(s)
}

/// Tell the org's user about a change of its openings.
pub async fn tell_openings(st: &AppState, org: &Org, what: &str, changes: &[p::SightChange]) {
    let rest = openings_text(st, org).await.unwrap_or_default();
    crate::notices::tell_user(
        st,
        org.id,
        &format!("Openings changed: {what}"),
        &format!(
            "{what}.\n\nWhat other orgs see changes as follows:\n{}\n{rest}\nOpenings are listed with `amail openings` and in the web interface (Other orgs).\n",
            changes_text(changes)
        ),
    )
    .await;
}

// --- teams -------------------------------------------------------------------

async fn teams_out(st: &AppState, org: &Org) -> ApiResult<Vec<p::Team>> {
    let teams = st.store.teams(org.id).await?;
    let seats = st.store.seats(org.id).await?;
    let grants = own_grants(st, org).await?;
    let mut open: HashMap<i64, Vec<String>> = HashMap::new();
    for g in &grants {
        if let GrantTarget::Team { team, .. } = &g.target {
            open.entry(team.id)
                .or_default()
                .push(render::audience(g.audience.as_deref()));
        }
    }
    Ok(teams
        .iter()
        .map(|t| {
            let members: Vec<&amail_store::Seat> =
                seats.iter().filter(|s| s.team_id == Some(t.id)).collect();
            p::Team {
                name: t.name.clone(),
                description: t.description.clone(),
                root: t.root.clone(),
                parent: t.parent.clone(),
                members: members.iter().map(|s| s.name.clone()).collect(),
                contacts: members
                    .iter()
                    .filter(|s| s.contact)
                    .map(|s| s.name.clone())
                    .collect(),
                open_to: open.remove(&t.id).unwrap_or_default(),
                managed: t.managed,
                created_at: render::ts(t.created_at),
            }
        })
        .collect())
}

/// `GET /v1/teams`.
pub async fn list_teams(State(st): State<AppState>, auth: Auth) -> ApiResult<Json<Vec<p::Team>>> {
    auth.require_directory()?;
    Ok(Json(teams_out(&st, &auth.org).await?))
}

/// `GET /v1/teams/{name}`.
pub async fn get_team(
    State(st): State<AppState>,
    auth: Auth,
    Path(name): Path<String>,
) -> ApiResult<Json<p::Team>> {
    auth.require_directory()?;
    teams_out(&st, &auth.org)
        .await?
        .into_iter()
        .find(|t| t.name == name)
        .map(Json)
        .ok_or_else(|| ApiError::not_found(format!("no team {name:?}")))
}

/// `PUT /v1/teams/{name}` (user): create or change a team.
pub async fn put_team(
    State(st): State<AppState>,
    auth: Auth,
    Path(name): Path<String>,
    Json(put): Json<p::TeamPut>,
) -> ApiResult<Json<p::Team>> {
    auth.require_user()?;
    crate::public::check_new_name(&st, &name)?;
    if let Some(Some(d)) = &put.description {
        p::check_description(d).map_err(ApiError::bad_request)?;
    }
    if let Some(cs) = &put.contacts {
        // An open team keeps at least one contact.
        let open = own_grants(&st, &auth.org)
            .await?
            .iter()
            .any(|g| matches!(&g.target, GrantTarget::Team { team, .. } if team.name == name));
        if cs.is_empty() && open {
            return Err(ApiError::unprocessable(
                "no_contacts",
                format!(
                    "team {name:?} is open to other orgs and needs at least one contact; close it first (amail close {name})"
                ),
            ));
        }
    }
    let change = TeamChange {
        description: put.description,
        root: put.root,
        parent: put.parent,
        contacts: put.contacts,
        managed: put.managed,
    };
    let (_, changes) = boundary::changing(&st, &auth.org, async {
        Ok(st.store.put_team(auth.org.id, &name, &change).await?)
    })
    .await?;
    st.directory_changed(auth.org.id, "team", &name, "updated");
    if !changes.is_empty() {
        tell_openings(
            &st,
            &auth.org,
            &format!("the contacts of team {name} changed"),
            &changes,
        )
        .await;
    }
    get_team(State(st), auth, Path(name)).await
}

/// `DELETE /v1/teams/{name}` (user): its members leave it; its grants go.
pub async fn delete_team(
    State(st): State<AppState>,
    auth: Auth,
    Path(name): Path<String>,
) -> ApiResult<StatusCode> {
    auth.require_user()?;
    let (found, changes) = boundary::changing(&st, &auth.org, async {
        Ok(st.store.delete_team(auth.org.id, &name).await?)
    })
    .await?;
    if !found {
        return Err(ApiError::not_found(format!("no team {name:?}")));
    }
    st.directory_changed(auth.org.id, "team", &name, "deleted");
    if !changes.is_empty() {
        tell_openings(
            &st,
            &auth.org,
            &format!("team {name} was deleted"),
            &changes,
        )
        .await;
    }
    Ok(StatusCode::NO_CONTENT)
}

// --- grants ------------------------------------------------------------------

/// `GET /v1/grants`: the org's openings.
pub async fn list_grants(State(st): State<AppState>, auth: Auth) -> ApiResult<Json<Vec<p::Grant>>> {
    auth.require_directory()?;
    let grants = own_grants(&st, &auth.org).await?;
    Ok(Json(grants_out(&st, &auth.org, &grants).await?))
}

/// A grant's audience from the request: `None` for every linked org.
fn audience(org: &Org, a: Option<&str>) -> ApiResult<Option<String>> {
    match a.map(str::trim) {
        None | Some("linked") | Some("") => Ok(None),
        Some(n) => {
            check_name("org", n)?;
            if n == org.name {
                return Err(ApiError::bad_request("an org cannot open seats to itself"));
            }
            Ok(Some(n.to_string()))
        }
    }
}

/// The target of a new grant, as it would be stored: a seat, or a team
/// with its contacts (which must not be empty).
async fn target(st: &AppState, org: &Org, name: &str) -> ApiResult<GrantTarget> {
    if let Some(s) = st.store.seat_by_name(org.id, name).await? {
        return Ok(GrantTarget::Seat(CardRec {
            id: s.id,
            name: s.name,
            kind: Some(s.kind),
            description: s.description,
        }));
    }
    let team = st
        .store
        .teams(org.id)
        .await?
        .into_iter()
        .find(|t| t.name == name)
        .ok_or_else(|| ApiError::not_found(format!("no seat or team {name:?}")))?;
    let contacts: Vec<CardRec> = st
        .store
        .seats(org.id)
        .await?
        .into_iter()
        .filter(|s| s.team_id == Some(team.id) && s.contact)
        .map(|s| CardRec {
            id: s.id,
            name: s.name,
            kind: Some(s.kind),
            description: s.description,
        })
        .collect();
    if contacts.is_empty() {
        return Err(ApiError::unprocessable(
            "no_contacts",
            format!(
                "team {name:?} has no contacts: mark at least one member as a contact first (amail team set {name} --contacts <seat>)"
            ),
        ));
    }
    Ok(GrantTarget::Team {
        team: CardRec {
            id: team.id,
            name: team.name,
            kind: None,
            description: team.description,
        },
        contacts,
    })
}

fn target_ids(t: &GrantTarget) -> (Option<i64>, Option<i64>) {
    match t {
        GrantTarget::Seat(s) => (Some(s.id), None),
        GrantTarget::Team { team, .. } => (None, Some(team.id)),
    }
}

/// `POST /v1/grants` (user): open a seat or team.
pub async fn add_grant(
    State(st): State<AppState>,
    auth: Auth,
    Json(req): Json<p::NewGrant>,
) -> ApiResult<(StatusCode, Json<p::GrantChange>)> {
    auth.require_user()?;
    let audience = audience(&auth.org, req.audience.as_deref())?;
    let t = target(&st, &auth.org, req.target.trim()).await?;
    let (seat_id, team_id) = target_ids(&t);
    let (id, changes) = boundary::changing(&st, &auth.org, async {
        Ok(st
            .store
            .add_grant(
                auth.org.id,
                seat_id,
                team_id,
                audience.as_deref(),
                req.managed,
            )
            .await?)
    })
    .await?;
    let grants = own_grants(&st, &auth.org).await?;
    let grant = grants_out(&st, &auth.org, &grants)
        .await?
        .into_iter()
        .find(|g| g.id == id);
    if let Some(g) = &grant {
        tell_openings(
            &st,
            &auth.org,
            &format!("{} opened to {}", g.target, g.audience),
            &changes,
        )
        .await;
    }
    st.directory_changed(auth.org.id, "grant", &req.target, "created");
    Ok((StatusCode::CREATED, Json(p::GrantChange { grant, changes })))
}

/// `DELETE /v1/grants/{id}` (user): close what the grant opened, at once
/// and in existing threads too.
pub async fn delete_grant(
    State(st): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
) -> ApiResult<Json<p::GrantChange>> {
    auth.require_user()?;
    let grants = own_grants(&st, &auth.org).await?;
    let g = grants
        .iter()
        .find(|g| g.id == id)
        .ok_or_else(|| ApiError::not_found(format!("no grant {id}")))?;
    let shown = render::grant(g, true);
    let (_, changes) = boundary::changing(&st, &auth.org, async {
        Ok(st.store.remove_grant(auth.org.id, id).await?)
    })
    .await?;
    tell_openings(
        &st,
        &auth.org,
        &format!("{} closed to {}", shown.target, shown.audience),
        &changes,
    )
    .await;
    st.directory_changed(auth.org.id, "grant", &shown.target, "deleted");
    Ok(Json(p::GrantChange {
        grant: Some(shown),
        changes,
    }))
}

/// `POST /v1/grants/preview` (user): what adding and removing these
/// grants would change, without changing anything.
pub async fn preview(
    State(st): State<AppState>,
    auth: Auth,
    Json(req): Json<p::GrantPreview>,
) -> ApiResult<Json<p::GrantChange>> {
    auth.require_user()?;
    if req.add.len() + req.remove.len() > 200 {
        return Err(ApiError::bad_request("at most 200 changes per preview"));
    }
    let grants = own_grants(&st, &auth.org).await?;
    let links = st.store.links(auth.org.id).await?;
    let before = boundary::shown_with(&auth.org, &grants, &links);
    let mut next: Vec<GrantRec> = grants
        .iter()
        .filter(|g| !req.remove.contains(&g.id))
        .cloned()
        .collect();
    for a in &req.add {
        next.push(GrantRec {
            id: 0,
            org_id: auth.org.id,
            org_name: auth.org.name.clone(),
            audience: audience(&auth.org, a.audience.as_deref())?,
            target: target(&st, &auth.org, a.target.trim()).await?,
            managed: a.managed,
            created_at: 0,
        });
    }
    let after = boundary::shown_with(&auth.org, &next, &links);
    Ok(Json(p::GrantChange {
        grant: None,
        changes: boundary::diff(&before, &after),
    }))
}

// --- other orgs --------------------------------------------------------------

fn org_card(o: &Org) -> p::OrgCard {
    p::OrgCard {
        name: o.name.clone(),
        display_name: o.display_name.clone(),
        description: o.description.clone(),
    }
}

/// `GET /v1/orgs/{name}`: another org as the caller's org sees it: its
/// card; with an active link, the cards it opened to the caller's org and
/// the caller's grants that include it. An org that is hidden from the
/// caller's org answers as a name no org has, after the same lookups.
pub async fn peer_org(
    State(st): State<AppState>,
    auth: Auth,
    Path(name): Path<String>,
) -> ApiResult<Json<p::PeerOrg>> {
    auth.require_directory()?;
    // Limited before the lookup, so that probing for org names counts.
    if let Err(ms) = st.limiters.card_lookups.take(&auth.org.id) {
        return Err(ApiError::too_many(
            "rate_limited",
            "too many org lookups from this org",
            ms,
        ));
    }
    let missing = || ApiError::not_found(format!("no org {name:?}"));
    if !valid_name(&name) {
        return Err(missing());
    }
    let org = st
        .store
        .org_by_name(&name)
        .await?
        .filter(|o| o.suspended_at.is_none());
    let links = st.store.links(auth.org.id).await?;
    let link = links.iter().find(|l| {
        l.peer_name == name
            && l.peer_org_id != 0
            && Some(l.peer_org_id) == org.as_ref().map(|o| o.id)
    });
    let Some(org) = org else {
        return Err(missing());
    };
    // A hidden org is seen by orgs it has an active link with, and by an
    // org it asked for a link itself.
    let state = link.map(|l| l.state);
    let visible = org.visibility == OrgVisibility::ListedByName
        || matches!(state, Some(LinkState::Active | LinkState::Invited));
    if !visible {
        return Err(missing());
    }
    let mut out = p::PeerOrg {
        card: org_card(&org),
        link: link.map(render::link),
        open_to_you: Vec::new(),
        you_open: Vec::new(),
    };
    if org.id == auth.org.id {
        return Ok(Json(out));
    }
    if state == Some(LinkState::Active) {
        out.open_to_you = boundary::sight(&st, &auth.org, Some(&org))
            .await?
            .remove(&org.name)
            .map(|s: OpenSet| s.cards())
            .unwrap_or_default();
        let grants = own_grants(&st, &auth.org).await?;
        out.you_open = grants_out(&st, &auth.org, &grants)
            .await?
            .into_iter()
            .filter(|g| g.audience == "linked" || g.audience == org.name)
            .collect();
    }
    Ok(Json(out))
}

/// `DELETE /v1/org/notice` (user): the one-time notice was shown.
pub async fn clear_notice(State(st): State<AppState>, auth: Auth) -> ApiResult<StatusCode> {
    auth.require_user()?;
    st.store.clear_org_notice(auth.org.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The audiences of each seat of the org (see [`render::open_to`]).
pub async fn seat_audiences(st: &AppState, org: &Org) -> ApiResult<HashMap<String, Vec<String>>> {
    Ok(render::open_to(&own_grants(st, org).await?))
}
