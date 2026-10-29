//! The per-seat counters behind `pending` and `counts`, and the thread
//! participation behind `org_sees_thread` and `seat_in_thread`, are kept by
//! the database in the transactions that change deliveries. These tests
//! change deliveries in every way the store does and compare the counters
//! with counts over the deliveries. Both backends (Postgres only when
//! `AMAIL_TEST_PG_URL` is set).

use amail_proto::{DeliveryClass as C, DeliveryMode as M, Priority};
use amail_store::testutil::{Backend, TestDb};
use amail_store::*;

macro_rules! store_tests {
    ($($name:ident),* $(,)?) => {
        mod sqlite {
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
                async fn $name() {
                    if let Some(db) = amail_store::testutil::open(super::Backend::Sqlite, stringify!($name)).await {
                        super::$name(db).await;
                    }
                }
            )*
        }
        mod postgres {
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
                async fn $name() {
                    if let Some(db) = amail_store::testutil::open(super::Backend::Postgres, stringify!($name)).await {
                        super::$name(db).await;
                    }
                }
            )*
        }
    };
}

store_tests!(
    counters_follow_deliveries,
    thread_visibility_lookups,
    released_and_deleted_claims
);

/// The counters of every seat as kept, and as counted over the deliveries,
/// and every org's stored and kept bytes as counted from scratch.
async fn check(db: &TestDb) {
    for c in db.storage_counts().await {
        assert_eq!(
            (c.stored_bytes, c.kept_bytes),
            (c.stored_recount, c.kept_recount),
            "org {}: storage counts",
            c.org_id
        );
    }
    let kept = db
        .rows_i64(
            "SELECT id, pending_to, pending_cc, pending_list, pending_urgent, pending_digest, unread, \
             COALESCE(pending_digest_since, -1) FROM seats ORDER BY id",
        )
        .await;
    let counted = db
        .rows_i64(
            "SELECT s.id, \
             (SELECT COUNT(*) FROM deliveries d WHERE d.seat_id = s.id AND d.notified_at IS NULL \
               AND d.mode = 'immediate' AND d.class = 'to'), \
             (SELECT COUNT(*) FROM deliveries d WHERE d.seat_id = s.id AND d.notified_at IS NULL \
               AND d.mode = 'immediate' AND d.class = 'cc'), \
             (SELECT COUNT(*) FROM deliveries d WHERE d.seat_id = s.id AND d.notified_at IS NULL \
               AND d.mode = 'immediate' AND d.class = 'list'), \
             (SELECT COUNT(*) FROM deliveries d JOIN messages m ON m.id = d.message_id \
               WHERE d.seat_id = s.id AND d.notified_at IS NULL AND d.mode = 'immediate' \
               AND m.priority = 'urgent'), \
             (SELECT COUNT(*) FROM deliveries d WHERE d.seat_id = s.id AND d.notified_at IS NULL \
               AND d.mode = 'digest'), \
             (SELECT COUNT(*) FROM deliveries d WHERE d.seat_id = s.id AND d.read_at IS NULL \
               AND d.mode <> 'archive'), \
             COALESCE((SELECT MIN(m.created_at) FROM deliveries d JOIN messages m ON m.id = d.message_id \
               WHERE d.seat_id = s.id AND d.notified_at IS NULL AND d.mode = 'digest'), -1) \
             FROM seats s ORDER BY s.id",
        )
        .await;
    assert_eq!(kept, counted);
}

fn msg(org_id: i64, from: i64, to: &[(&Seat, C, M)]) -> NewMessage {
    NewMessage {
        org_id,
        sender_seat_id: from,
        thread_id: None,
        parent_id: None,
        depth: 0,
        subject: "s".into(),
        body: "b".into(),
        to_addrs: vec![],
        cc_addrs: vec![],
        priority: Priority::Normal,
        ack_required: false,
        meta: Default::default(),
        deliveries: to
            .iter()
            .map(|(s, c, m)| NewDelivery {
                seat_id: s.id,
                org_id: s.org_id,
                class: *c,
                mode: *m,
            })
            .collect(),
        ack_due: None,
        written_by: None,
        storage_quota: None,
        notice: false,
        guards: Vec::new(),
    }
}

fn claim(classes: &[C], digest: bool, urgent_only: bool) -> ClaimFilter {
    ClaimFilter {
        classes: classes.to_vec(),
        urgent_only,
        include_digest: digest,
        via: "test".into(),
        limit: 2,
        read_body_max: Some(100),
        digest_only: false,
        key: None,
    }
}

async fn counters_follow_deliveries(db: TestDb) {
    let s = &db.store;
    let (acme, owner) = s.create_org("acme", "owner", "h1").await.unwrap();
    let (other, other_owner) = s.create_org("other", "owner", "h2").await.unwrap();
    let mut seats = Vec::new();
    for n in ["a", "b", "c"] {
        seats.push(
            s.create_seat(
                acme.id,
                &NewSeat {
                    name: n.into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap(),
        );
    }
    let x = s
        .create_seat(
            other.id,
            &NewSeat {
                name: "x".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let (a, b, c) = (&seats[0], &seats[1], &seats[2]);
    let mut ids = Vec::new();
    for i in 0..12 {
        let mut m = msg(
            acme.id,
            owner.id,
            &[
                (a, C::To, M::Immediate),
                (b, C::Cc, M::Immediate),
                (c, C::List, [M::Immediate, M::Digest, M::Archive][i % 3]),
                (&x, C::List, M::Immediate),
            ],
        );
        if i % 4 == 0 {
            m.priority = Priority::Urgent;
        }
        ids.push(s.send(&m).await.unwrap().message_id);
        check(&db).await;
    }
    // A message from the other org, and one the owner sends to itself.
    s.send(&msg(other.id, other_owner.id, &[(a, C::To, M::Immediate)]))
        .await
        .unwrap();
    s.send(&msg(acme.id, owner.id, &[(&owner, C::To, M::Immediate)]))
        .await
        .unwrap();
    check(&db).await;
    // Claims of several kinds (each marks short bodies read).
    s.claim(a.id, &claim(&[C::To], false, true)).await.unwrap();
    check(&db).await;
    s.claim(a.id, &claim(&[C::To], false, false)).await.unwrap();
    check(&db).await;
    s.claim(c.id, &claim(&[C::List], true, false))
        .await
        .unwrap();
    check(&db).await;
    let mut f = claim(&[C::Cc, C::List], true, false);
    f.digest_only = true;
    f.read_body_max = None;
    s.claim(c.id, &f).await.unwrap();
    check(&db).await;
    let mut f = claim(&[C::Cc], false, false);
    f.key = Some("k".into());
    f.read_body_max = None;
    s.claim(b.id, &f).await.unwrap();
    s.claim(b.id, &f).await.unwrap();
    check(&db).await;
    // Withdrawing, twice each: c's deliveries of three messages (one of
    // each mode) and a's and b's (claimed, so
    // they stay as they are).
    let stored = db.storage_counts().await;
    let mut withdrawn = 0;
    for id in &ids[6..9] {
        for seat in [a, b, c] {
            let (d, bump) = s.withdraw(*id, seat.id).await.unwrap().unwrap();
            withdrawn += i64::from(bump.is_some());
            assert_eq!(bump.is_some(), d.withdrawn_at.is_some());
            let (_, again) = s.withdraw(*id, seat.id).await.unwrap().unwrap();
            assert!(again.is_none());
        }
        check(&db).await;
    }
    assert!(withdrawn > 0);
    assert!(s.withdraw(ids[0], other_owner.id).await.unwrap().is_none());
    // Storage is not given back: the deliveries stay.
    let after = db.storage_counts().await;
    assert_eq!(
        stored.iter().map(|c| c.stored_bytes).collect::<Vec<_>>(),
        after.iter().map(|c| c.stored_bytes).collect::<Vec<_>>()
    );
    // Reading and acknowledging, twice each.
    for id in &ids[..5] {
        s.mark_read(*id, b.id).await.unwrap();
        s.mark_read(*id, b.id).await.unwrap();
        s.ack(*id, c.id).await.unwrap();
        s.ack(*id, c.id).await.unwrap();
        s.ack(*id, x.id).await.unwrap();
    }
    check(&db).await;
    // Reading many at once: some of c's by id, then all of b's.
    s.mark_read_many(c.id, &ReadSelection::Ids(ids[5..9].to_vec()))
        .await
        .unwrap();
    check(&db).await;
    s.mark_read_many(b.id, &ReadSelection::All { before_seq: None })
        .await
        .unwrap();
    check(&db).await;
    // The counts the endpoints read.
    let p = s.pending(acme.id, Some(a.id)).await.unwrap();
    let counted = db
        .rows_i64(&format!(
            "SELECT COUNT(*) FROM deliveries WHERE seat_id = {} AND notified_at IS NULL AND mode = 'immediate' AND class = 'to'",
            a.id
        ))
        .await;
    assert_eq!(p[0].to, counted[0][0]);
    let digest = s.pending(acme.id, Some(c.id)).await.unwrap();
    let oldest = db
        .rows_i64(&format!(
            "SELECT MIN(m.created_at) FROM deliveries d JOIN messages m ON m.id = d.message_id \
             WHERE d.seat_id = {} AND d.notified_at IS NULL AND d.mode = 'digest'",
            c.id
        ))
        .await;
    assert!(digest[0].digest > 0);
    assert_eq!(digest[0].digest_since, Some(oldest[0][0]));
    let counts = s.counts(acme.id, None).await.unwrap();
    for r in &counts {
        let row = db
            .rows_i64(&format!(
                "SELECT pending_to + pending_cc + pending_list, unread FROM seats WHERE id = {}",
                r.seat_id
            ))
            .await;
        assert_eq!((r.pending, r.unread), (row[0][0], row[0][1]));
    }
    // Deleting a seat keeps its mail; deleting an org removes its
    // deliveries, and the other org's counters stay right.
    s.delete_seat(acme.id, "b").await.unwrap();
    check(&db).await;
    assert!(s.delete_org(acme.id).await.unwrap());
    check(&db).await;
    let left = db
        .rows_i64(&format!(
            "SELECT pending_list, unread FROM seats WHERE id = {}",
            x.id
        ))
        .await;
    // `x` acknowledged 5 of its 12 list deliveries: acknowledged (and read)
    // deliveries count as notified and leave the pending counts.
    assert_eq!(left[0], vec![7, 7]);
}

async fn thread_visibility_lookups(db: TestDb) {
    let s = &db.store;
    let (acme, owner) = s.create_org("acme", "owner", "h1").await.unwrap();
    let (bravo, bravo_owner) = s.create_org("bravo", "owner", "h2").await.unwrap();
    let (_, charlie_owner) = s.create_org("charlie", "owner", "h3").await.unwrap();
    let a = s
        .create_seat(
            acme.id,
            &NewSeat {
                name: "a".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let root = s
        .send(&msg(acme.id, owner.id, &[(&a, C::To, M::Immediate)]))
        .await
        .unwrap();
    // Bravo receives a reply in the thread; Charlie nothing.
    let mut reply = msg(acme.id, a.id, &[(&bravo_owner, C::To, M::Immediate)]);
    reply.thread_id = Some(root.thread_id);
    reply.parent_id = Some(root.message_id);
    reply.depth = 1;
    s.send(&reply).await.unwrap();
    let t = root.thread_id;
    assert!(s.org_sees_thread(acme.id, t).await.unwrap());
    assert!(s.org_sees_thread(bravo.id, t).await.unwrap());
    assert!(!s.org_sees_thread(charlie_owner.org_id, t).await.unwrap());
    assert!(!s.org_sees_thread(acme.id, t + 1000).await.unwrap());
    assert!(s.seat_in_thread(owner.id, t).await.unwrap());
    assert!(s.seat_in_thread(a.id, t).await.unwrap());
    assert!(s.seat_in_thread(bravo_owner.id, t).await.unwrap());
    assert!(!s.seat_in_thread(charlie_owner.id, t).await.unwrap());
    // Bravo's view of the thread is the reply only; Charlie's is empty.
    let seen = s.thread(t, bravo.id, 10, 1 << 20).await.unwrap();
    assert_eq!(seen.len(), 1);
    assert!(
        s.thread(t, charlie_owner.org_id, 10, 1 << 20)
            .await
            .unwrap()
            .is_empty()
    );
    // After Bravo is deleted its participation is gone.
    assert!(s.delete_org(bravo.id).await.unwrap());
    let left = db
        .rows_i64(&format!(
            "SELECT COUNT(*) FROM thread_seats WHERE org_id = {}",
            bravo.id
        ))
        .await;
    assert_eq!(left[0][0], 0);
}

/// A failed turn releases the mail of its claim: the counters and storage
/// counts follow. The user deletes a thread whose mail a seat has claimed
/// in a turn that is still going on: the deliveries go, the seat's counter
/// moves (so that its node and clients look again), and releasing or
/// replaying the claim afterwards finds nothing of the deleted mail.
async fn released_and_deleted_claims(db: TestDb) {
    let s = &db.store;
    let (acme, owner) = s.create_org("acme", "owner", "h1").await.unwrap();
    let a = s
        .create_seat(
            acme.id,
            &NewSeat {
                name: "a".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let mut ids = Vec::new();
    for i in 0..4 {
        let mut m = msg(acme.id, owner.id, &[(&a, C::To, M::Immediate)]);
        if i == 3 {
            m.priority = Priority::Urgent;
        }
        ids.push(s.send(&m).await.unwrap());
    }
    check(&db).await;
    let mut f = claim(&[C::To], false, false);
    f.limit = 10;
    f.key = Some("turn-1".into());
    let (taken, _) = s.claim(a.id, &f).await.unwrap();
    assert_eq!(taken.len(), 4);
    check(&db).await;
    // Failed turn: two of the four go back to pending, unread.
    let (released, bump) = s
        .release_claim(a.id, "turn-1", &[ids[0].message_id, ids[3].message_id])
        .await
        .unwrap();
    assert_eq!(released, vec![ids[0].message_id, ids[3].message_id]);
    assert!(bump.is_some());
    check(&db).await;
    let p = s.pending(acme.id, Some(a.id)).await.unwrap();
    assert_eq!((p[0].to, p[0].urgent), (2, 1));
    // Released twice: nothing more.
    let (again, _) = s
        .release_claim(a.id, "turn-1", &[ids[0].message_id])
        .await
        .unwrap();
    assert!(again.is_empty());
    check(&db).await;

    // Claimed again in a new turn; the user deletes the thread of message 1
    // (internal mail) while that turn goes on.
    f.key = Some("turn-2".into());
    let (taken, _) = s.claim(a.id, &f).await.unwrap();
    assert_eq!(taken.len(), 2);
    let before = s.seat_seqs(acme.id).await.unwrap();
    let seq_before = before.iter().find(|r| r.0 == a.id).unwrap().2;
    let done = s
        .delete_thread(acme.id, ids[0].thread_id, 1000)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(done.deleted, 1);
    assert_eq!(done.seqs.len(), 1);
    assert_eq!(done.seqs[0].seat_id, a.id);
    assert!(done.seqs[0].seq > seq_before);
    check(&db).await;
    // The turn fails: its other message is released, the deleted one is
    // not there to release.
    let (released, _) = s
        .release_claim(a.id, "turn-2", &[ids[0].message_id, ids[3].message_id])
        .await
        .unwrap();
    assert_eq!(released, vec![ids[3].message_id]);
    check(&db).await;
    // A replay of the first turn's claim returns what is left of it.
    f.key = Some("turn-1".into());
    let (replay, _) = s.claim(a.id, &f).await.unwrap();
    let mut left: Vec<i64> = replay.iter().map(|e| e.message.id).collect();
    left.sort_unstable();
    assert_eq!(
        left,
        vec![ids[1].message_id, ids[2].message_id, ids[3].message_id]
    );
    check(&db).await;
}
