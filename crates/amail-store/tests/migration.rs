//! Migration 0018 (M7) on data written by the schema before it: groups
//! become teams, user seats open to linked orgs and nothing else, existing
//! links stay active and their orgs get a one-time notice, and existing
//! cross-org threads record their participants.

use amail_store::testutil::{Backend, fresh_url};
use amail_store::{GrantFilter, GrantTarget, OpenOptions};
use sqlx::Connection;
use std::borrow::Cow;

/// Statements creating M6 data: orgs alpha (user, two seats in group
/// `backend`, one ungrouped) and beta (user, one seat), an active link, and
/// a message from alpha's `a1` to beta's `b1` (thread 1).
const OLD_DATA: &[&str] = &[
    "INSERT INTO orgs (id, name, created_at) VALUES (1, 'alpha', 0), (2, 'beta', 0)",
    "INSERT INTO seats (id, org_id, name, kind, created_at) VALUES (1, 1, 'owner', 'user', 0), (2, 2, 'owner', 'user', 0)",
    "INSERT INTO seats (id, org_id, name, kind, group_name, created_at) VALUES \
     (3, 1, 'a1', 'agent', 'backend', 0), (4, 1, 'a2', 'agent', 'backend', 0), \
     (5, 1, 'solo', 'agent', NULL, 0), (6, 2, 'b1', 'agent', NULL, 0)",
    "INSERT INTO links (org_id, peer_org_id, state, created_at) VALUES (1, 2, 'active', 0), (2, 1, 'active', 0)",
    "INSERT INTO messages (id, org_id, sender_seat_id, thread_id, depth, subject, body, lines, to_addrs, cc_addrs, \
     priority, ack_required, created_at) VALUES (1, 1, 3, 1, 0, 's', 'b', 1, 'b1@beta', '', 'normal', FALSE, 0)",
    "INSERT INTO deliveries (org_id, message_id, seat_id, class, mode, seq, mod_seq) VALUES (2, 1, 6, 'to', 'immediate', 1, 1)",
];

async fn migrate(backend: Backend) {
    let Some((url, _dir)) = fresh_url(backend, "migration_0018").await else {
        return;
    };
    // The schema before M7, then the old data.
    match backend {
        Backend::Sqlite => {
            let mut m = sqlx::migrate!("./migrations/sqlite");
            let mut c = sqlx::SqliteConnection::connect(&format!("{url}?mode=rwc"))
                .await
                .unwrap();
            m.migrations = Cow::Owned(
                m.migrations
                    .iter()
                    .filter(|x| x.version < 18)
                    .cloned()
                    .collect(),
            );
            m.run(&mut c).await.unwrap();
            for s in OLD_DATA {
                sqlx::raw_sql(s).execute(&mut c).await.expect(s);
            }
        }
        Backend::Postgres => {
            let mut m = sqlx::migrate!("./migrations/postgres");
            let mut c = sqlx::PgConnection::connect(&url).await.unwrap();
            m.migrations = Cow::Owned(
                m.migrations
                    .iter()
                    .filter(|x| x.version < 18)
                    .cloned()
                    .collect(),
            );
            m.run(&mut c).await.unwrap();
            for s in OLD_DATA {
                sqlx::raw_sql(s).execute(&mut c).await.expect(s);
            }
            for t in ["orgs", "seats", "messages", "deliveries"] {
                sqlx::raw_sql(&format!(
                    "SELECT setval(pg_get_serial_sequence('{t}', 'id'), (SELECT MAX(id) FROM {t}))"
                ))
                .execute(&mut c)
                .await
                .unwrap();
            }
        }
    }
    // Opening the store runs the remaining migrations.
    let store = amail_store::open(&url, &OpenOptions::default())
        .await
        .unwrap();
    let alpha = store.org_by_name("alpha").await.unwrap().unwrap();
    let beta = store.org_by_name("beta").await.unwrap().unwrap();
    // Groups became teams.
    let teams = store.teams(alpha.id).await.unwrap();
    assert_eq!(teams.len(), 1);
    assert_eq!(teams[0].name, "backend");
    assert!(teams[0].managed);
    let seats = store.seats(alpha.id).await.unwrap();
    for s in &seats {
        let want = if s.name.starts_with("a") {
            Some("backend")
        } else {
            None
        };
        assert_eq!(s.group.as_deref(), want, "{}", s.name);
        assert_eq!(s.team_id.is_some(), want.is_some(), "{}", s.name);
    }
    // Only user seats are open.
    for org in [&alpha, &beta] {
        let g = store
            .grants(&GrantFilter {
                grantor: Some(org.id),
                audience: None,
            })
            .await
            .unwrap();
        assert_eq!(g.len(), 1, "{}", org.name);
        assert!(matches!(&g[0].target, GrantTarget::Seat(c) if c.name == "owner"));
        assert_eq!(g[0].audience, None);
    }
    // The link stays active; its orgs are told once.
    assert!(store.link_active(alpha.id, beta.id).await.unwrap());
    assert!(
        alpha
            .notice
            .as_deref()
            .unwrap()
            .contains("Links no longer open")
    );
    store.clear_org_notice(alpha.id).await.unwrap();
    assert!(
        store
            .org_by_name("alpha")
            .await
            .unwrap()
            .unwrap()
            .notice
            .is_none()
    );
    // The thread's participants.
    let p = store.thread_peers(beta.id, &[1]).await.unwrap();
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].card.name, "a1");
    assert!(store.thread_peers(alpha.id, &[1]).await.unwrap().is_empty());
}

#[tokio::test]
async fn migration_0018_sqlite() {
    migrate(Backend::Sqlite).await;
}

#[tokio::test]
async fn migration_0018_postgres() {
    migrate(Backend::Postgres).await;
}
