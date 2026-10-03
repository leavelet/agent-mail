//! Store conformance tests. Every test runs against SQLite and Postgres
//! (Postgres only when `AMAIL_TEST_PG_URL` is set; otherwise it is skipped
//! with a notice).

use amail_proto::{
    DeliveryClass as C, DeliveryMode as M, LinkState, PostPolicy, Priority, SeatKind,
};
use amail_store::*;
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

type S = Arc<dyn Store>;

macro_rules! conformance {
    ($($name:ident),* $(,)?) => {
        mod sqlite {
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
                async fn $name() {
                    if let Some(db) = amail_store::testutil::open(amail_store::testutil::Backend::Sqlite, stringify!($name)).await {
                        super::$name(db.store.clone()).await;
                    }
                }
            )*
        }
        mod postgres {
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
                async fn $name() {
                    if let Some(db) = amail_store::testutil::open(amail_store::testutil::Backend::Postgres, stringify!($name)).await {
                        super::$name(db.store.clone()).await;
                    }
                }
            )*
        }
    };
}

conformance!(
    orgs_and_tokens,
    seats,
    seat_and_list_names_share_namespace,
    lists_and_members,
    links,
    send_inbox_read_ack,
    threads_and_visibility,
    changes_feed,
    archive,
    claim_filters,
    claim_concurrent_never_duplicates,
    seq_concurrent_sends_monotonic,
    seq_mixed_workload_feed_complete,
);

async fn org(s: &S, name: &str) -> (Org, Seat) {
    s.create_org(name, "owner", &format!("hash-{name}"))
        .await
        .unwrap()
}

async fn seat(s: &S, org_id: i64, name: &str) -> Seat {
    s.create_seat(
        org_id,
        &NewSeat {
            name: name.into(),
            ..Default::default()
        },
    )
    .await
    .unwrap()
}

fn msg(org_id: i64, from: i64, subject: &str, to: &[(&Seat, C)]) -> NewMessage {
    NewMessage {
        org_id,
        sender_seat_id: from,
        thread_id: None,
        parent_id: None,
        depth: 0,
        subject: subject.into(),
        body: format!("body of {subject}\nsecond line"),
        to_addrs: vec![],
        cc_addrs: vec![],
        priority: Priority::Normal,
        ack_required: false,
        deliveries: to
            .iter()
            .map(|(s, c)| NewDelivery {
                seat_id: s.id,
                org_id: s.org_id,
                class: *c,
                mode: M::Immediate,
            })
            .collect(),
    }
}

fn filter(classes: &[C]) -> ClaimFilter {
    ClaimFilter {
        classes: classes.to_vec(),
        urgent_only: false,
        include_digest: false,
        via: "test".into(),
        limit: 100,
    }
}

async fn orgs_and_tokens(s: S) {
    let (o, u) = org(&s, "acme").await;
    assert_eq!(o.name, "acme");
    assert_eq!(o.send_rate_per_min, 30);
    assert_eq!(o.max_recipients, 200);
    assert_eq!(o.max_thread_depth, 50);
    assert_eq!(u.kind, SeatKind::User);
    assert_eq!(u.name, "owner");
    assert!(matches!(
        s.create_org("acme", "owner", "h2").await,
        Err(StoreError::Conflict(_))
    ));

    let a = s.token_auth("hash-acme").await.unwrap().unwrap();
    assert_eq!(a.scope, amail_proto::TokenScope::User);
    assert_eq!(a.org.id, o.id);
    assert_eq!(a.seat.unwrap().id, u.id);
    assert!(s.token_auth("nope").await.unwrap().is_none());

    s.create_token(&NewToken {
        org_id: o.id,
        scope: amail_proto::TokenScope::Node,
        seat_id: None,
        hash: "node-h".into(),
        label: None,
    })
    .await
    .unwrap();
    let n = s.token_auth("node-h").await.unwrap().unwrap();
    assert_eq!(n.scope, amail_proto::TokenScope::Node);
    assert!(n.seat.is_none());

    let o2 = s
        .update_org(
            o.id,
            &OrgSettings {
                send_rate_per_min: Some(5),
                max_recipients: Some(3),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        (o2.send_rate_per_min, o2.send_burst, o2.max_recipients),
        (5, 30, 3)
    );
    assert_eq!(s.org_by_name("acme").await.unwrap().unwrap(), o2);
    assert!(s.org_by_name("nope").await.unwrap().is_none());
}

async fn seats(s: S) {
    let (o, _) = org(&s, "acme").await;
    let lead = seat(&s, o.id, "lead").await;
    let w = s
        .create_seat(
            o.id,
            &NewSeat {
                name: "w1".into(),
                parent: Some("lead".into()),
                role: Some("worker".into()),
                harness: Some("claude".into()),
                model: Some("sonnet".into()),
                display_name: Some("Worker 1".into()),
            },
        )
        .await
        .unwrap();
    assert_eq!(w.parent_id, Some(lead.id));
    let found = s
        .seats_by_names(o.id, &["w1".into(), "lead".into(), "ghost".into()])
        .await
        .unwrap();
    let mut names: Vec<_> = found.iter().map(|s| s.name.as_str()).collect();
    names.sort();
    assert_eq!(names, vec!["lead", "w1"]);
    assert!(s.seats_by_names(o.id, &[]).await.unwrap().is_empty());
    assert_eq!(w.parent_name.as_deref(), Some("lead"));
    assert_eq!(w.kind, SeatKind::Agent);
    assert!(matches!(
        s.create_seat(
            o.id,
            &NewSeat {
                name: "x".into(),
                parent: Some("ghost".into()),
                ..Default::default()
            }
        )
        .await,
        Err(StoreError::Invalid(_))
    ));
    assert!(matches!(
        s.create_seat(
            o.id,
            &NewSeat {
                name: "w1".into(),
                ..Default::default()
            }
        )
        .await,
        Err(StoreError::Conflict(_))
    ));

    // Update: clear role, keep harness, reject a parent cycle.
    let w = s
        .update_seat(
            o.id,
            "w1",
            &SeatUpdate {
                role: Some(None),
                model: Some(Some("opus".into())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(w.role, None);
    assert_eq!(w.harness.as_deref(), Some("claude"));
    assert_eq!(w.model.as_deref(), Some("opus"));
    assert!(matches!(
        s.update_seat(
            o.id,
            "lead",
            &SeatUpdate {
                parent: Some(Some("w1".into())),
                ..Default::default()
            }
        )
        .await,
        Err(StoreError::Invalid(_))
    ));
    assert!(matches!(
        s.update_seat(
            o.id,
            "lead",
            &SeatUpdate {
                parent: Some(Some("lead".into())),
                ..Default::default()
            }
        )
        .await,
        Err(StoreError::Invalid(_))
    ));

    // Token, list membership, then soft delete.
    s.create_token(&NewToken {
        org_id: o.id,
        scope: amail_proto::TokenScope::Seat,
        seat_id: Some(lead.id),
        hash: "lead-h".into(),
        label: None,
    })
    .await
    .unwrap();
    let l = s
        .create_list(
            o.id,
            &NewList {
                name: "team".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    s.put_member(
        o.id,
        l.id,
        &Member {
            address: "lead".into(),
            seat_id: Some(lead.id),
            mode: M::Immediate,
        },
    )
    .await
    .unwrap();
    s.delete_seat(o.id, "lead").await.unwrap();
    assert!(s.seat_by_name(o.id, "lead").await.unwrap().is_none());
    assert!(s.token_auth("lead-h").await.unwrap().is_none());
    assert!(s.list_members(l.id).await.unwrap().is_empty());
    let w = s.seat_by_name(o.id, "w1").await.unwrap().unwrap();
    assert_eq!(
        w.parent_id, None,
        "children move to the deleted seat's parent"
    );
    let names: Vec<_> = s
        .seats(o.id)
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(names, vec!["owner", "w1"]);
    // The name can be reused; the user seat cannot be deleted.
    seat(&s, o.id, "lead").await;
    assert!(matches!(
        s.delete_seat(o.id, "owner").await,
        Err(StoreError::Invalid(_))
    ));
    assert!(matches!(
        s.delete_seat(o.id, "ghost").await,
        Err(StoreError::NotFound(_))
    ));
}

async fn seat_and_list_names_share_namespace(s: S) {
    let (o, _) = org(&s, "acme").await;
    let (o2, _) = org(&s, "beta").await;
    seat(&s, o.id, "dev").await;
    assert!(matches!(
        s.create_list(
            o.id,
            &NewList {
                name: "dev".into(),
                ..Default::default()
            }
        )
        .await,
        Err(StoreError::Conflict(_))
    ));
    s.create_list(
        o.id,
        &NewList {
            name: "ops".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        s.create_seat(
            o.id,
            &NewSeat {
                name: "ops".into(),
                ..Default::default()
            }
        )
        .await,
        Err(StoreError::Conflict(_))
    ));
    // Other orgs have their own namespace.
    seat(&s, o2.id, "dev").await;
    s.create_list(
        o2.id,
        &NewList {
            name: "ops".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    // Concurrent creation of the same name: exactly one wins.
    let mut tasks = Vec::new();
    for i in 0..10 {
        let s = s.clone();
        let oid = o.id;
        tasks.push(tokio::spawn(async move {
            if i % 2 == 0 {
                s.create_seat(
                    oid,
                    &NewSeat {
                        name: "race".into(),
                        ..Default::default()
                    },
                )
                .await
                .is_ok()
            } else {
                s.create_list(
                    oid,
                    &NewList {
                        name: "race".into(),
                        ..Default::default()
                    },
                )
                .await
                .is_ok()
            }
        }));
    }
    let mut ok = 0;
    for t in tasks {
        ok += t.await.unwrap() as i32;
    }
    assert_eq!(ok, 1);
}

async fn lists_and_members(s: S) {
    let (o, _) = org(&s, "acme").await;
    let a = seat(&s, o.id, "a").await;
    let l = s
        .create_list(
            o.id,
            &NewList {
                name: "team".into(),
                description: Some("d".into()),
                post_policy: Some(PostPolicy::Members),
            },
        )
        .await
        .unwrap();
    assert_eq!(l.post_policy, PostPolicy::Members);
    assert_eq!(l.member_count, 0);
    s.put_member(
        o.id,
        l.id,
        &Member {
            address: "a".into(),
            seat_id: Some(a.id),
            mode: M::Immediate,
        },
    )
    .await
    .unwrap();
    s.put_member(
        o.id,
        l.id,
        &Member {
            address: "x@other".into(),
            seat_id: None,
            mode: M::Digest,
        },
    )
    .await
    .unwrap();
    // Upsert changes the mode.
    s.put_member(
        o.id,
        l.id,
        &Member {
            address: "a".into(),
            seat_id: Some(a.id),
            mode: M::Archive,
        },
    )
    .await
    .unwrap();
    let m = s.list_members(l.id).await.unwrap();
    assert_eq!(
        m,
        vec![
            Member {
                address: "a".into(),
                seat_id: Some(a.id),
                mode: M::Archive
            },
            Member {
                address: "x@other".into(),
                seat_id: None,
                mode: M::Digest
            },
        ]
    );
    assert_eq!(
        s.list_by_name(o.id, "team")
            .await
            .unwrap()
            .unwrap()
            .member_count,
        2
    );
    assert!(s.remove_member(l.id, "x@other").await.unwrap());
    assert!(!s.remove_member(l.id, "x@other").await.unwrap());
    assert_eq!(s.lists(o.id).await.unwrap().len(), 1);
    s.delete_list(o.id, "team").await.unwrap();
    assert!(s.list_by_name(o.id, "team").await.unwrap().is_none());
    assert!(matches!(
        s.delete_list(o.id, "team").await,
        Err(StoreError::NotFound(_))
    ));
}

async fn links(s: S) {
    let (a, _) = org(&s, "a").await;
    let (b, _) = org(&s, "b").await;
    let (c, _) = org(&s, "c").await;
    assert!(!s.link_active(a.id, b.id).await.unwrap());
    assert_eq!(
        s.request_link(a.id, b.id).await.unwrap(),
        LinkState::Requested
    );
    assert_eq!(s.links(a.id).await.unwrap()[0].state, LinkState::Requested);
    assert_eq!(s.links(b.id).await.unwrap()[0].state, LinkState::Invited);
    assert!(!s.link_active(a.id, b.id).await.unwrap());
    // The requester cannot accept its own request.
    assert!(matches!(
        s.accept_link(a.id, b.id).await,
        Err(StoreError::NotFound(_))
    ));
    s.accept_link(b.id, a.id).await.unwrap();
    assert!(s.link_active(a.id, b.id).await.unwrap());
    assert!(s.link_active(b.id, a.id).await.unwrap());
    // A request in the other direction counts as acceptance.
    assert_eq!(
        s.request_link(a.id, c.id).await.unwrap(),
        LinkState::Requested
    );
    assert_eq!(s.request_link(c.id, a.id).await.unwrap(), LinkState::Active);
    assert!(s.link_active(a.id, c.id).await.unwrap());
    // Either side can revoke.
    assert!(s.revoke_link(b.id, a.id).await.unwrap());
    assert!(!s.link_active(a.id, b.id).await.unwrap());
    assert!(s.links(b.id).await.unwrap().is_empty());
    assert!(!s.revoke_link(b.id, a.id).await.unwrap());
    assert!(matches!(
        s.request_link(a.id, a.id).await,
        Err(StoreError::Invalid(_))
    ));
    // `b` and `c` were never linked.
    assert!(!s.link_active(b.id, c.id).await.unwrap());
}

async fn send_inbox_read_ack(s: S) {
    let (o, u) = org(&s, "acme").await;
    let a = seat(&s, o.id, "a").await;
    let b = seat(&s, o.id, "b").await;
    let mut m = msg(o.id, u.id, "hello", &[(&a, C::To), (&b, C::Cc)]);
    m.to_addrs = vec!["a@acme".into()];
    m.cc_addrs = vec!["b@acme".into()];
    m.ack_required = true;
    m.priority = Priority::Urgent;
    let sent = s.send(&m).await.unwrap();
    assert_eq!(sent.thread_id, sent.message_id);
    assert_eq!(sent.seqs.len(), 2);
    assert!(sent.seqs.iter().all(|b| b.seq == 1));

    let inbox = s
        .inbox(
            a.id,
            &InboxQuery {
                unread_only: true,
                limit: 50,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(inbox.len(), 1);
    let e = &inbox[0];
    assert_eq!(e.delivery.class, C::To);
    assert_eq!(e.delivery.seq, 1);
    assert_eq!(e.delivery.seat_name, "a");
    assert_eq!(e.message.subject, "hello");
    assert_eq!(e.message.body, "", "inbox carries headers only");
    assert_eq!(e.message.lines, 2);
    assert_eq!(e.message.sender_name, "owner");
    assert_eq!(e.message.sender_kind, SeatKind::User);
    assert_eq!(e.message.to_addrs, vec!["a@acme"]);
    assert_eq!(e.message.cc_addrs, vec!["b@acme"]);
    assert_eq!(e.message.priority, Priority::Urgent);
    assert!(e.message.ack_required);

    let full = s.message(sent.message_id).await.unwrap().unwrap();
    assert_eq!(full.body, "body of hello\nsecond line");

    let (d, bump) = s.mark_read(sent.message_id, a.id).await.unwrap().unwrap();
    assert!(d.read_at.is_some());
    assert_eq!(bump.unwrap().seq, 2);
    assert_eq!(d.mod_seq, 2);
    assert_eq!(d.seq, 1);
    // Reading again changes nothing.
    let (_, bump) = s.mark_read(sent.message_id, a.id).await.unwrap().unwrap();
    assert!(bump.is_none());
    assert!(
        s.inbox(
            a.id,
            &InboxQuery {
                unread_only: true,
                limit: 50,
                ..Default::default()
            }
        )
        .await
        .unwrap()
        .is_empty()
    );
    assert_eq!(
        s.inbox(
            a.id,
            &InboxQuery {
                limit: 50,
                ..Default::default()
            }
        )
        .await
        .unwrap()
        .len(),
        1
    );

    // Ack implies read; a non-recipient has no delivery.
    let (d, bump) = s.ack(sent.message_id, b.id).await.unwrap().unwrap();
    assert!(d.acked_at.is_some() && d.read_at.is_some());
    assert!(bump.is_some());
    let (_, bump) = s.ack(sent.message_id, b.id).await.unwrap().unwrap();
    assert!(bump.is_none());
    assert!(s.mark_read(sent.message_id, u.id).await.unwrap().is_none());
    assert!(s.ack(sent.message_id, u.id).await.unwrap().is_none());

    let r = s.recipients(sent.message_id).await.unwrap();
    assert_eq!(
        r.iter().map(|d| d.seat_name.as_str()).collect::<Vec<_>>(),
        vec!["a", "b"]
    );

    // Inbox paging.
    for i in 0..5 {
        s.send(&msg(o.id, u.id, &format!("m{i}"), &[(&a, C::To)]))
            .await
            .unwrap();
    }
    let all = s
        .inbox(
            a.id,
            &InboxQuery {
                limit: 50,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let seqs: Vec<i64> = all.iter().map(|e| e.delivery.seq).collect();
    assert!(seqs.windows(2).all(|w| w[0] < w[1]));
    let page = s
        .inbox(
            a.id,
            &InboxQuery {
                after_seq: Some(seqs[2]),
                limit: 2,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        page.iter().map(|e| e.delivery.seq).collect::<Vec<_>>(),
        seqs[3..5].to_vec()
    );
    let tail = s
        .inbox(
            a.id,
            &InboxQuery {
                before_seq: Some(i64::MAX),
                limit: 2,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        tail.iter().map(|e| e.delivery.seq).collect::<Vec<_>>(),
        seqs[4..6].to_vec()
    );
}

async fn threads_and_visibility(s: S) {
    let (o, u) = org(&s, "acme").await;
    let (o2, u2) = org(&s, "beta").await;
    let a = seat(&s, o.id, "a").await;
    let b = seat(&s, o.id, "b").await;
    let c = seat(&s, o.id, "c").await;
    let x = seat(&s, o2.id, "x").await;
    let y = seat(&s, o2.id, "y").await;

    let root = s
        .send(&msg(o.id, a.id, "root", &[(&b, C::To), (&x, C::To)]))
        .await
        .unwrap();
    let mut reply = msg(o2.id, x.id, "re", &[(&a, C::To)]);
    reply.thread_id = Some(root.thread_id);
    reply.parent_id = Some(root.message_id);
    reply.depth = 1;
    let reply = s.send(&reply).await.unwrap();
    assert_eq!(reply.thread_id, root.thread_id);
    // Internal to beta, in the same thread.
    let mut internal = msg(o2.id, x.id, "internal", &[(&y, C::To)]);
    internal.thread_id = Some(root.thread_id);
    let internal = s.send(&internal).await.unwrap();
    // Internal to acme, between b and c.
    let mut side = msg(o.id, b.id, "side", &[(&c, C::To)]);
    side.thread_id = Some(root.thread_id);
    let side = s.send(&side).await.unwrap();

    let ids = |v: Vec<MessageRec>| v.into_iter().map(|m| m.id).collect::<Vec<_>>();
    assert_eq!(
        ids(s.thread(root.thread_id, o.id).await.unwrap()),
        vec![root.message_id, reply.message_id, side.message_id]
    );
    assert_eq!(
        ids(s.thread(root.thread_id, o2.id).await.unwrap()),
        vec![root.message_id, reply.message_id, internal.message_id]
    );
    assert!(s.org_sees_message(o.id, reply.message_id).await.unwrap());
    assert!(!s.org_sees_message(o.id, internal.message_id).await.unwrap());
    assert!(s.org_sees_message(o2.id, root.message_id).await.unwrap());
    assert!(!s.org_sees_message(o2.id, side.message_id).await.unwrap());

    assert!(s.seat_in_thread(a.id, root.thread_id).await.unwrap());
    assert!(s.seat_in_thread(c.id, root.thread_id).await.unwrap());
    assert!(s.seat_in_thread(y.id, root.thread_id).await.unwrap());
    assert!(!s.seat_in_thread(u.id, root.thread_id).await.unwrap());
    assert!(!s.seat_in_thread(u2.id, root.thread_id).await.unwrap());

    let m = s.message(reply.message_id).await.unwrap().unwrap();
    assert_eq!((m.org_name.as_str(), m.sender_name.as_str()), ("beta", "x"));
    assert_eq!(m.parent_id, Some(root.message_id));
    assert_eq!(m.depth, 1);
}

async fn changes_feed(s: S) {
    let (o, u) = org(&s, "acme").await;
    let a = seat(&s, o.id, "a").await;
    let m1 = s.send(&msg(o.id, u.id, "1", &[(&a, C::To)])).await.unwrap();
    let m2 = s.send(&msg(o.id, u.id, "2", &[(&a, C::Cc)])).await.unwrap();
    let ch = s.changes(a.id, 0, 100).await.unwrap();
    assert_eq!(
        ch.iter().map(|e| e.delivery.mod_seq).collect::<Vec<_>>(),
        vec![1, 2]
    );
    s.mark_read(m1.message_id, a.id).await.unwrap();
    let ch = s.changes(a.id, 2, 100).await.unwrap();
    assert_eq!(ch.len(), 1);
    assert_eq!(ch[0].delivery.message_id, m1.message_id);
    assert_eq!((ch[0].delivery.seq, ch[0].delivery.mod_seq), (1, 3));
    let (claimed, bump) = s
        .claim(a.id, &filter(&[C::To, C::Cc, C::List]))
        .await
        .unwrap();
    assert_eq!(claimed.len(), 2);
    assert_eq!(bump.unwrap().seq, 5);
    let ch = s.changes(a.id, 3, 100).await.unwrap();
    assert_eq!(
        ch.iter().map(|e| e.delivery.mod_seq).collect::<Vec<_>>(),
        vec![4, 5]
    );
    assert_eq!(ch[1].delivery.message_id, m2.message_id);
    assert!(s.changes(a.id, 5, 100).await.unwrap().is_empty());
    assert_eq!(s.changes(a.id, 0, 1).await.unwrap().len(), 1);
    let seqs = s.seat_seqs(o.id).await.unwrap();
    assert!(seqs.contains(&(a.id, "a".to_string(), 5)));
}

async fn archive(s: S) {
    let (o, u) = org(&s, "acme").await;
    let (o2, _) = org(&s, "beta").await;
    let (o3, _) = org(&s, "gamma").await;
    let a = seat(&s, o.id, "a").await;
    let x = seat(&s, o2.id, "x").await;
    let z = seat(&s, o3.id, "z").await;
    let m1 = s.send(&msg(o.id, a.id, "1", &[(&x, C::To)])).await.unwrap();
    let m2 = s
        .send(&msg(o2.id, x.id, "2", &[(&a, C::To)]))
        .await
        .unwrap();
    let _m3 = s
        .send(&msg(o2.id, x.id, "3", &[(&z, C::To)]))
        .await
        .unwrap();
    let m4 = s.send(&msg(o.id, u.id, "4", &[(&a, C::To)])).await.unwrap();

    let q = |f: ArchiveQuery| {
        let s = s.clone();
        let limit = if f.limit == 0 { 50 } else { f.limit };
        async move {
            s.archive(o.id, &ArchiveQuery { limit, ..f })
                .await
                .unwrap()
                .into_iter()
                .map(|m| m.id)
                .collect::<Vec<_>>()
        }
    };
    assert_eq!(
        q(ArchiveQuery::default()).await,
        vec![m4.message_id, m2.message_id, m1.message_id]
    );
    assert_eq!(
        q(ArchiveQuery {
            from: Some(("beta".into(), "x".into())),
            ..Default::default()
        })
        .await,
        vec![m2.message_id]
    );
    assert_eq!(
        q(ArchiveQuery {
            thread_id: Some(m1.thread_id),
            ..Default::default()
        })
        .await,
        vec![m1.message_id]
    );
    assert_eq!(
        q(ArchiveQuery {
            before_id: Some(m4.message_id),
            limit: 1,
            ..Default::default()
        })
        .await,
        vec![m2.message_id]
    );
    assert_eq!(
        q(ArchiveQuery {
            since: Some(m4.created_at + 1),
            ..Default::default()
        })
        .await,
        Vec::<i64>::new()
    );
    assert_eq!(
        q(ArchiveQuery {
            until: Some(m1.created_at),
            ..Default::default()
        })
        .await,
        Vec::<i64>::new()
    );
    let all = s
        .archive(
            o.id,
            &ArchiveQuery {
                limit: 50,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(all.iter().all(|m| m.body.is_empty()));
    assert_eq!(
        s.archive(
            o3.id,
            &ArchiveQuery {
                limit: 50,
                ..Default::default()
            }
        )
        .await
        .unwrap()
        .len(),
        1
    );
}

async fn claim_filters(s: S) {
    let (o, u) = org(&s, "acme").await;
    let a = seat(&s, o.id, "a").await;
    let send = |subject: &'static str, class: C, mode: M, urgent: bool| {
        let s = s.clone();
        let mut m = msg(o.id, u.id, subject, &[(&a, class)]);
        m.deliveries[0].mode = mode;
        if urgent {
            m.priority = Priority::Urgent;
        }
        async move { s.send(&m).await.unwrap().message_id }
    };
    let to = send("to", C::To, M::Immediate, false).await;
    let cc = send("cc", C::Cc, M::Immediate, false).await;
    let list = send("list", C::List, M::Immediate, false).await;
    let digest = send("digest", C::List, M::Digest, false).await;
    let archive = send("archive", C::List, M::Archive, false).await;
    let urgent = send("urgent", C::Cc, M::Immediate, true).await;

    let ids = |v: Vec<Entry>| v.into_iter().map(|e| e.message.id).collect::<Vec<_>>();
    let (got, _) = s
        .claim(
            a.id,
            &ClaimFilter {
                urgent_only: true,
                ..filter(&[C::To, C::Cc, C::List])
            },
        )
        .await
        .unwrap();
    assert_eq!(ids(got), vec![urgent]);
    let (got, _) = s.claim(a.id, &filter(&[C::To])).await.unwrap();
    assert_eq!(got[0].delivery.notified_via.as_deref(), Some("test"));
    assert!(got[0].delivery.notified_at.is_some());
    assert_eq!(
        got[0].message.body, "body of to\nsecond line",
        "claim returns bodies"
    );
    assert_eq!(ids(got), vec![to]);
    let (got, _) = s
        .claim(
            a.id,
            &ClaimFilter {
                limit: 1,
                ..filter(&[C::Cc, C::List])
            },
        )
        .await
        .unwrap();
    assert_eq!(ids(got), vec![cc]);
    let (got, _) = s.claim(a.id, &filter(&[C::Cc, C::List])).await.unwrap();
    assert_eq!(ids(got), vec![list]);
    let (got, _) = s
        .claim(
            a.id,
            &ClaimFilter {
                include_digest: true,
                ..filter(&[C::List])
            },
        )
        .await
        .unwrap();
    assert_eq!(ids(got), vec![digest]);
    let (got, bump) = s
        .claim(
            a.id,
            &ClaimFilter {
                include_digest: true,
                ..filter(&[C::To, C::Cc, C::List])
            },
        )
        .await
        .unwrap();
    assert!(
        got.is_empty() && bump.is_none(),
        "archive-mode deliveries are never claimed"
    );
    let d = s.delivery(archive, a.id).await.unwrap().unwrap();
    assert!(d.notified_at.is_none());
    let (got, _) = s.claim(a.id, &filter(&[])).await.unwrap();
    assert!(got.is_empty());
}

/// Many concurrent claimers on one seat while new mail keeps arriving: every
/// delivery is returned exactly once.
async fn claim_concurrent_never_duplicates(s: S) {
    let (o, u) = org(&s, "acme").await;
    let a = seat(&s, o.id, "a").await;
    const INITIAL: usize = 300;
    const LATER: usize = 200;
    for i in 0..INITIAL {
        s.send(&msg(o.id, u.id, &format!("m{i}"), &[(&a, C::To)]))
            .await
            .unwrap();
    }
    let sender = {
        let (s, a) = (s.clone(), a.clone());
        tokio::spawn(async move {
            for i in 0..LATER {
                s.send(&msg(o.id, u.id, &format!("late{i}"), &[(&a, C::Cc)]))
                    .await
                    .unwrap();
            }
        })
    };
    let mut claimers = Vec::new();
    for _ in 0..16 {
        let s = s.clone();
        let seat_id = a.id;
        claimers.push(tokio::spawn(async move {
            let mut got = Vec::new();
            let mut idle = 0;
            while idle < 50 {
                let (v, _) = s
                    .claim(
                        seat_id,
                        &ClaimFilter {
                            limit: 7,
                            ..filter(&[C::To, C::Cc])
                        },
                    )
                    .await
                    .unwrap();
                if v.is_empty() {
                    idle += 1;
                    tokio::task::yield_now().await;
                } else {
                    idle = 0;
                    got.extend(v.into_iter().map(|e| e.delivery.id));
                }
            }
            got
        }));
    }
    sender.await.unwrap();
    let mut all = Vec::new();
    for c in claimers {
        all.extend(c.await.unwrap());
    }
    // Drain anything that arrived after the claimers went idle.
    loop {
        let (v, _) = s.claim(a.id, &filter(&[C::To, C::Cc])).await.unwrap();
        if v.is_empty() {
            break;
        }
        all.extend(v.into_iter().map(|e| e.delivery.id));
    }
    let unique: HashSet<i64> = all.iter().copied().collect();
    assert_eq!(unique.len(), all.len(), "a delivery was claimed twice");
    assert_eq!(all.len(), INITIAL + LATER);
}

/// Concurrent sends to overlapping seat sets: each seat's seqs are exactly
/// 1..=N, and an incremental reader following the `changes` cursor while
/// the sends happen sees every delivery (no out-of-order visibility).
async fn seq_concurrent_sends_monotonic(s: S) {
    let (o, u) = org(&s, "acme").await;
    let mut seats = Vec::new();
    for i in 0..6 {
        seats.push(seat(&s, o.id, &format!("s{i}")).await);
    }
    let seats = Arc::new(seats);
    const SENDERS: usize = 12;
    const PER_SENDER: usize = 40;
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let watcher = {
        let (s, target, done) = (s.clone(), seats[0].clone(), done.clone());
        tokio::spawn(async move {
            let mut cursor = 0;
            let mut seen = HashSet::new();
            loop {
                let finished = done.load(std::sync::atomic::Ordering::SeqCst);
                let page = s.changes(target.id, cursor, 1000).await.unwrap();
                for e in &page {
                    assert!(e.delivery.mod_seq > cursor);
                    cursor = e.delivery.mod_seq;
                    seen.insert(e.delivery.id);
                }
                if finished && page.is_empty() {
                    return seen;
                }
            }
        })
    };

    let mut tasks = Vec::new();
    for t in 0..SENDERS {
        let (s, seats) = (s.clone(), seats.clone());
        tasks.push(tokio::spawn(async move {
            for i in 0..PER_SENDER {
                // Overlapping recipient sets in varying order; seat 0 always included.
                let k = (t + i) % 5 + 1;
                let mut rcpt = vec![(&seats[0], C::To)];
                for j in 0..k {
                    rcpt.push((&seats[(t + i + j) % 5 + 1], C::Cc));
                }
                rcpt.dedup_by_key(|(s, _)| s.id);
                let mut seen = HashSet::new();
                rcpt.retain(|(s, _)| seen.insert(s.id));
                s.send(&msg(o.id, u.id, &format!("t{t}-{i}"), &rcpt))
                    .await
                    .unwrap();
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    done.store(true, std::sync::atomic::Ordering::SeqCst);
    let seen = watcher.await.unwrap();

    for st in seats.iter() {
        let all = s
            .inbox(
                st.id,
                &InboxQuery {
                    limit: 100_000,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let seqs: Vec<i64> = all.iter().map(|e| e.delivery.seq).collect();
        let expected: Vec<i64> = (1..=seqs.len() as i64).collect();
        assert_eq!(seqs, expected, "seat {} seqs not contiguous", st.name);
        let last = s
            .seat_by_name(o.id, &st.name)
            .await
            .unwrap()
            .unwrap()
            .last_seq;
        assert_eq!(last, seqs.len() as i64);
    }
    let all0: HashSet<i64> = s
        .inbox(
            seats[0].id,
            &InboxQuery {
                limit: 100_000,
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.delivery.id)
        .collect();
    assert_eq!(all0.len(), SENDERS * PER_SENDER);
    assert_eq!(seen, all0, "incremental reader missed deliveries");
    let _ = u;
}

/// Sends, claims, reads and acks race on the same seats. Every change takes a
/// distinct seq, and a reader following `changes` ends with the latest state
/// of every delivery.
async fn seq_mixed_workload_feed_complete(s: S) {
    let (o, u) = org(&s, "acme").await;
    let a = seat(&s, o.id, "a").await;
    let b = seat(&s, o.id, "b").await;
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let watcher = {
        let (s, a, done) = (s.clone(), a.clone(), done.clone());
        tokio::spawn(async move {
            let mut cursor = 0;
            let mut state: BTreeMap<i64, i64> = BTreeMap::new(); // delivery id -> mod_seq
            loop {
                let finished = done.load(std::sync::atomic::Ordering::SeqCst);
                let page = s.changes(a.id, cursor, 500).await.unwrap();
                for e in &page {
                    assert!(e.delivery.mod_seq > cursor, "changes went backwards");
                    cursor = e.delivery.mod_seq;
                    state.insert(e.delivery.id, e.delivery.mod_seq);
                }
                if finished && page.is_empty() {
                    return state;
                }
            }
        })
    };

    let mut tasks = Vec::new();
    for t in 0..6 {
        let (s, a, b) = (s.clone(), a.clone(), b.clone());
        tasks.push(tokio::spawn(async move {
            for i in 0..40 {
                let sent = s
                    .send(&msg(
                        o.id,
                        u.id,
                        &format!("{t}-{i}"),
                        &[(&a, C::To), (&b, C::Cc)],
                    ))
                    .await
                    .unwrap();
                match i % 4 {
                    0 => {
                        s.mark_read(sent.message_id, a.id).await.unwrap();
                    }
                    1 => {
                        s.ack(sent.message_id, a.id).await.unwrap();
                    }
                    _ => {
                        s.claim(
                            a.id,
                            &ClaimFilter {
                                limit: 3,
                                ..filter(&[C::To])
                            },
                        )
                        .await
                        .unwrap();
                    }
                }
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    done.store(true, std::sync::atomic::Ordering::SeqCst);
    let state = watcher.await.unwrap();

    let all = s.changes(a.id, 0, 100_000).await.unwrap();
    assert_eq!(all.len(), 240);
    let mods: Vec<i64> = all.iter().map(|e| e.delivery.mod_seq).collect();
    assert!(
        mods.windows(2).all(|w| w[0] < w[1]),
        "mod_seq not strictly increasing"
    );
    let last = s.seat_by_name(o.id, "a").await.unwrap().unwrap().last_seq;
    assert_eq!(*mods.last().unwrap(), last);
    let fresh: BTreeMap<i64, i64> = all
        .iter()
        .map(|e| (e.delivery.id, e.delivery.mod_seq))
        .collect();
    assert_eq!(
        state, fresh,
        "incremental reader diverged from the final state"
    );
}
