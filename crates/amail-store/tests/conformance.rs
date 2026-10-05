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
    empty_send_rejected,
    meta_round_trip,
    pending_counts,
    token_list_and_revoke,
    seat_permission_mode,
    claim_marks_inlined_read,
    claim_keys_concurrent,
    claim_release,
    presence_store,
    send_budget_exact,
    ack_steps_taken_once,
    system_seat_hidden,
    focus_and_digest_pending,
    search_steps_are_bounded,
    grants_and_thread_peers,
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
        meta: Default::default(),
        deliveries: to
            .iter()
            .map(|(s, c)| NewDelivery {
                seat_id: s.id,
                org_id: s.org_id,
                class: *c,
                mode: M::Immediate,
            })
            .collect(),
        ack_due: None,
        storage_quota: None,
        notice: false,
    }
}

fn filter(classes: &[C]) -> ClaimFilter {
    ClaimFilter {
        classes: classes.to_vec(),
        urgent_only: false,
        include_digest: false,
        via: "test".into(),
        limit: 100,
        read_body_max: None,
        digest_only: false,
        key: None,
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
                permission_mode: Some(amail_proto::PermissionMode::Unattended),
                group: Some("backend".into()),
                description: Some("Builds the API.".into()),
                contact: true,
            },
        )
        .await
        .unwrap();
    assert_eq!(w.parent_id, Some(lead.id));
    assert_eq!(w.group.as_deref(), Some("backend"));
    // The group can be changed and cleared like the other fields.
    let w2 = s
        .update_seat(
            o.id,
            "w1",
            &SeatUpdate {
                group: Some(Some("frontend".into())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(w2.group.as_deref(), Some("frontend"));
    assert_eq!(w2.role.as_deref(), Some("worker"));
    let w3 = s
        .update_seat(
            o.id,
            "w1",
            &SeatUpdate {
                group: Some(None),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(w3.group, None);
    s.update_seat(
        o.id,
        "w1",
        &SeatUpdate {
            group: Some(Some("backend".into())),
            ..Default::default()
        },
    )
    .await
    .unwrap();
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
                managed: false,
            },
        )
        .await
        .unwrap();
    assert_eq!(l.post_policy, PostPolicy::Members);
    assert!(!l.managed);
    let generated = s
        .create_list(
            o.id,
            &NewList {
                name: "generated".into(),
                managed: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(generated.managed);
    assert!(
        s.lists(o.id)
            .await
            .unwrap()
            .iter()
            .any(|l| l.name == "generated" && l.managed)
    );
    s.delete_list(o.id, "generated").await.unwrap();
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
        s.request_link(a.id, b.id, None).await.unwrap(),
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
        s.request_link(a.id, c.id, None).await.unwrap(),
        LinkState::Requested
    );
    assert_eq!(
        s.request_link(c.id, a.id, None).await.unwrap(),
        LinkState::Active
    );
    assert!(s.link_active(a.id, c.id).await.unwrap());
    // Either side can revoke.
    assert!(s.revoke_link(b.id, a.id).await.unwrap());
    assert!(!s.link_active(a.id, b.id).await.unwrap());
    assert!(s.links(b.id).await.unwrap().is_empty());
    assert!(!s.revoke_link(b.id, a.id).await.unwrap());
    assert!(matches!(
        s.request_link(a.id, a.id, None).await,
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

    let ids = |v: Vec<amail_store::ThreadMessage>| {
        v.into_iter().map(|t| t.message.id).collect::<Vec<_>>()
    };
    assert_eq!(
        ids(s
            .thread(root.thread_id, o.id, 1000, usize::MAX)
            .await
            .unwrap()),
        vec![root.message_id, reply.message_id, side.message_id]
    );
    assert_eq!(
        ids(s
            .thread(root.thread_id, o2.id, 1000, usize::MAX)
            .await
            .unwrap()),
        vec![root.message_id, reply.message_id, internal.message_id]
    );
    assert_eq!(
        ids(s.thread(root.thread_id, o.id, 2, usize::MAX).await.unwrap()),
        vec![reply.message_id, side.message_id]
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
                .messages
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
        .unwrap()
        .messages;
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
        .messages
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

async fn empty_send_rejected(s: S) {
    let (o, u) = org(&s, "acme").await;
    let m = msg(o.id, u.id, "nobody", &[]);
    assert!(matches!(s.send(&m).await, Err(StoreError::Invalid(_))));
    // Nothing was stored.
    let a = s
        .archive(
            o.id,
            &ArchiveQuery {
                limit: 10,
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .messages;
    assert!(a.is_empty(), "{a:?}");
}

async fn meta_round_trip(s: S) {
    let (o, u) = org(&s, "acme").await;
    let a = seat(&s, o.id, "a").await;
    let mut m = msg(o.id, u.id, "merge", &[(&a, C::To)]);
    m.meta = BTreeMap::from([
        ("repo".to_string(), "core".to_string()),
        ("commit".to_string(), "abc\"123".to_string()),
    ]);
    let sent = s.send(&m).await.unwrap();
    let got = s.message(sent.message_id).await.unwrap().unwrap();
    assert_eq!(got.meta, m.meta);
    let plain = s
        .send(&msg(o.id, u.id, "plain", &[(&a, C::To)]))
        .await
        .unwrap();
    assert!(
        s.message(plain.message_id)
            .await
            .unwrap()
            .unwrap()
            .meta
            .is_empty()
    );
    // Headers (inbox, claim) carry meta too.
    let (claimed, _) = s.claim(a.id, &filter(&[C::To])).await.unwrap();
    assert_eq!(claimed.len(), 2);
    assert_eq!(claimed[0].message.meta, m.meta);
}

async fn pending_counts(s: S) {
    let (o, u) = org(&s, "acme").await;
    let a = seat(&s, o.id, "a").await;
    let b = seat(&s, o.id, "b").await;
    s.send(&msg(o.id, u.id, "1", &[(&a, C::To), (&b, C::Cc)]))
        .await
        .unwrap();
    let mut urgent = msg(o.id, u.id, "2", &[(&a, C::Cc)]);
    urgent.priority = Priority::Urgent;
    s.send(&urgent).await.unwrap();
    let mut digest = msg(o.id, u.id, "3", &[(&a, C::List)]);
    digest.deliveries[0].mode = M::Digest;
    s.send(&digest).await.unwrap();

    let all = s.pending(o.id, None).await.unwrap();
    let names: Vec<&str> = all.iter().map(|p| p.seat_name.as_str()).collect();
    assert_eq!(names, ["a", "b", "owner"]);
    let pa = &all[0];
    assert_eq!((pa.to, pa.cc, pa.list, pa.urgent), (1, 1, 0, 1));
    assert_eq!(pa.last_seq, 3);
    let pb = &all[1];
    assert_eq!((pb.to, pb.cc, pb.list, pb.urgent), (0, 1, 0, 0));
    assert_eq!(
        all[2],
        PendingRec {
            seat_id: u.id,
            seat_name: "owner".into(),
            user: true,
            ..Default::default()
        }
    );

    s.claim(a.id, &filter(&[C::To])).await.unwrap();
    let one = s.pending(o.id, Some(a.id)).await.unwrap();
    assert_eq!(one.len(), 1);
    assert_eq!((one[0].to, one[0].cc, one[0].urgent), (0, 1, 1));
    assert_eq!(one[0].last_seq, 4);
}

async fn token_list_and_revoke(s: S) {
    let (o, u) = org(&s, "acme").await;
    let (o2, _) = org(&s, "beta").await;
    let a = seat(&s, o.id, "a").await;
    for (scope, seat, hash, label) in [
        (
            amail_proto::TokenScope::Seat,
            Some(a.id),
            "h-a",
            Some("agent a"),
        ),
        (amail_proto::TokenScope::Node, None, "h-n", None),
    ] {
        s.create_token(&NewToken {
            org_id: o.id,
            scope,
            seat_id: seat,
            hash: hash.into(),
            label: label.map(str::to_string),
        })
        .await
        .unwrap();
    }
    let t = s.tokens(o.id).await.unwrap();
    assert_eq!(t.len(), 3);
    assert_eq!(t[0].scope, amail_proto::TokenScope::User);
    assert_eq!(t[0].seat_name.as_deref(), Some(u.name.as_str()));
    assert_eq!(t[1].seat_name.as_deref(), Some("a"));
    assert_eq!(t[1].label.as_deref(), Some("agent a"));
    assert!(t[2].seat_name.is_none());
    // Other orgs cannot revoke it; revoking works once.
    assert!(!s.revoke_token(o2.id, t[1].id).await.unwrap());
    assert!(s.revoke_token(o.id, t[1].id).await.unwrap());
    assert!(!s.revoke_token(o.id, t[1].id).await.unwrap());
    assert!(s.token_auth("h-a").await.unwrap().is_none());
    assert_eq!(s.tokens(o.id).await.unwrap().len(), 2);
}

async fn seat_permission_mode(s: S) {
    use amail_proto::PermissionMode as P;
    let (o, _) = org(&s, "acme").await;
    let a = seat(&s, o.id, "a").await;
    assert_eq!(a.permission_mode, None);
    let set = |v: Option<P>| SeatUpdate {
        permission_mode: Some(v),
        ..Default::default()
    };
    let a = s
        .update_seat(o.id, "a", &set(Some(P::Unattended)))
        .await
        .unwrap();
    assert_eq!(a.permission_mode, Some(P::Unattended));
    // Untouched by unrelated patches.
    let a = s
        .update_seat(
            o.id,
            "a",
            &SeatUpdate {
                role: Some(Some("r".into())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(a.permission_mode, Some(P::Unattended));
    let a = s.update_seat(o.id, "a", &set(None)).await.unwrap();
    assert_eq!(a.permission_mode, None);
}

/// A claim can mark read, in the same change, the deliveries whose whole
/// body the caller shows.
async fn claim_marks_inlined_read(s: S) {
    let (o, u) = org(&s, "acme").await;
    let a = seat(&s, o.id, "a").await;
    let mut short = msg(o.id, u.id, "short", &[(&a, C::To)]);
    short.body = "é".repeat(10); // characters, not bytes
    let mut long = msg(o.id, u.id, "long", &[(&a, C::To)]);
    long.body = "x".repeat(11);
    let s1 = s.send(&short).await.unwrap();
    let s2 = s.send(&long).await.unwrap();
    let before = s.seat_seqs(o.id).await.unwrap();
    let mut f = filter(&[C::To]);
    f.read_body_max = Some(10);
    let (got, bump) = s.claim(a.id, &f).await.unwrap();
    assert_eq!(got.len(), 2);
    let short_d = s.delivery(s1.message_id, a.id).await.unwrap().unwrap();
    let long_d = s.delivery(s2.message_id, a.id).await.unwrap().unwrap();
    assert!(short_d.notified_at.is_some() && short_d.read_at.is_some());
    assert!(long_d.notified_at.is_some() && long_d.read_at.is_none());
    assert_eq!(got[0].delivery.read_at, short_d.read_at);
    // One sequence step per delivery, as for a plain claim.
    let a_before = before.iter().find(|x| x.0 == a.id).unwrap().2;
    assert_eq!(bump.unwrap().seq, a_before + 2);
    // Without the option nothing is marked read.
    let s3 = s.send(&short).await.unwrap();
    s.claim(a.id, &filter(&[C::To])).await.unwrap();
    assert!(
        s.delivery(s3.message_id, a.id)
            .await
            .unwrap()
            .unwrap()
            .read_at
            .is_none()
    );
}

async fn claim_keys_concurrent(s: S) {
    let (o, user) = org(&s, "acme").await;
    let a = seat(&s, o.id, "a").await;
    for i in 0..20 {
        s.send(&msg(o.id, user.id, &format!("m{i}"), &[(&a, C::To)]))
            .await
            .unwrap();
    }
    // Eight concurrent claims with one key, eight with another: each key's
    // claims all return the same batch, and the batches do not overlap.
    let mut tasks = Vec::new();
    for i in 0..16 {
        let s = s.clone();
        let mut f = filter(&[C::To]);
        f.limit = 5;
        f.key = Some(if i % 2 == 0 { "k-even" } else { "k-odd" }.into());
        tasks.push(tokio::spawn(async move {
            let (e, _) = s.claim(a.id, &f).await.unwrap();
            (i % 2, e.iter().map(|e| e.delivery.id).collect::<Vec<_>>())
        }));
    }
    let mut by_key: BTreeMap<i32, HashSet<Vec<i64>>> = BTreeMap::new();
    for t in tasks {
        let (k, ids) = t.await.unwrap();
        by_key.entry(k).or_default().insert(ids);
    }
    let even: Vec<_> = by_key[&0].iter().collect();
    let odd: Vec<_> = by_key[&1].iter().collect();
    assert_eq!((even.len(), odd.len()), (1, 1), "{by_key:?}");
    assert_eq!((even[0].len(), odd[0].len()), (5, 5));
    assert!(even[0].iter().all(|x| !odd[0].contains(x)));
    // Ten left; a keyed claim after the fact still returns its batch.
    let mut f = filter(&[C::To]);
    f.key = Some("k-even".into());
    let (e, bump) = s.claim(a.id, &f).await.unwrap();
    assert_eq!(
        &e.iter().map(|e| e.delivery.id).collect::<Vec<_>>(),
        even[0]
    );
    assert!(bump.is_none());
    assert!(e.iter().all(|e| e.delivery.notified_at.is_some()));
    let f = filter(&[C::To]);
    assert_eq!(s.claim(a.id, &f).await.unwrap().0.len(), 10);
    // A key used by one seat means nothing for another.
    let b = seat(&s, o.id, "b").await;
    s.send(&msg(o.id, user.id, "for b", &[(&b, C::To)]))
        .await
        .unwrap();
    let mut f = filter(&[C::To]);
    f.key = Some("k-even".into());
    assert_eq!(s.claim(b.id, &f).await.unwrap().0.len(), 1);
}

/// A keyed claim's deliveries go back to "not notified, not read" when
/// released, except acknowledged ones and messages the claim did not take.
async fn claim_release(s: S) {
    let (o, user) = org(&s, "acme").await;
    let a = seat(&s, o.id, "a").await;
    let mut ids = Vec::new();
    for i in 0..4 {
        let m = msg(o.id, user.id, &format!("m{i}"), &[(&a, C::To)]);
        ids.push(s.send(&m).await.unwrap().message_id);
    }
    let mut f = filter(&[C::To]);
    f.limit = 3;
    f.key = Some("k1".into());
    f.read_body_max = Some(1000);
    let (got, _) = s.claim(a.id, &f).await.unwrap();
    assert_eq!(got.len(), 3);
    let before = s.seat_seqs(o.id).await.unwrap();
    let a_before = before.iter().find(|x| x.0 == a.id).unwrap().2;
    // Acknowledged meanwhile: kept.
    s.ack(ids[1], a.id).await.unwrap();
    let (released, bump) = s
        .release_claim(a.id, "k1", &[ids[0], ids[1], ids[3]])
        .await
        .unwrap();
    // ids[3] was not taken by this claim.
    assert_eq!(released, vec![ids[0]]);
    assert!(bump.unwrap().seq > a_before);
    let d0 = s.delivery(ids[0], a.id).await.unwrap().unwrap();
    assert!(d0.notified_at.is_none() && d0.read_at.is_none());
    assert!(d0.notified_via.is_none());
    let d1 = s.delivery(ids[1], a.id).await.unwrap().unwrap();
    assert!(d1.notified_at.is_some() && d1.acked_at.is_some());
    let d2 = s.delivery(ids[2], a.id).await.unwrap().unwrap();
    assert!(d2.notified_at.is_some());
    // Counted as pending again: a new claim takes it (and the untouched one).
    let (again, _) = s.claim(a.id, &filter(&[C::To])).await.unwrap();
    let mut got: Vec<i64> = again.iter().map(|e| e.message.id).collect();
    got.sort();
    assert_eq!(got, vec![ids[0], ids[3]]);
    // Unknown key, or another seat: nothing.
    assert!(
        s.release_claim(a.id, "nope", &[ids[0]])
            .await
            .unwrap()
            .0
            .is_empty()
    );
    let b = seat(&s, o.id, "b").await;
    assert!(
        s.release_claim(b.id, "k1", &[ids[2]])
            .await
            .unwrap()
            .0
            .is_empty()
    );
}

async fn presence_store(s: S) {
    use amail_proto::SeatState;
    let (o, _) = org(&s, "acme").await;
    let _a = seat(&s, o.id, "a").await;
    let rec = |seat: &str, state: SeatState, since: i64| PresenceRec {
        seat: seat.into(),
        state,
        wake_failed: false,
        harness: Some("claude".into()),
        node: Some("h1".into()),
        since,
        updated_at: 0,
        attention: None,
        attention_hint: None,
        terminal_url: None,
        turn_failed: None,
    };
    let now = now_ms();
    let changed = s
        .put_presence(
            o.id,
            &[
                rec("a", SeatState::Busy, now - 5000),
                rec("ghost", SeatState::Idle, now),
            ],
            90_000,
        )
        .await
        .unwrap()
        .changed;
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0].since, now - 5000);
    // Same state again: no change, `since` kept, `updated_at` refreshed.
    let changed = s
        .put_presence(o.id, &[rec("a", SeatState::Busy, now)], 90_000)
        .await
        .unwrap()
        .changed;
    assert!(changed.is_empty());
    let p = s.presence(o.id, Some("a")).await.unwrap();
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].since, now - 5000);
    assert!(p[0].updated_at >= now);
    let changed = s
        .put_presence(o.id, &[rec("a", SeatState::Idle, now)], 90_000)
        .await
        .unwrap()
        .changed;
    assert_eq!(changed[0].state, SeatState::Idle);
    // A stale entry counts as absent: the same state is a change again.
    let changed = s
        .put_presence(o.id, &[rec("a", SeatState::Idle, now)], -1)
        .await
        .unwrap()
        .changed;
    assert_eq!(changed.len(), 1);
    // A turn failure is stored and is a change.
    let mut failed = rec("a", SeatState::Idle, now);
    failed.turn_failed = Some(amail_proto::TurnFailure {
        reason: "model error".into(),
        at: "2026-10-05T00:00:00.000Z".into(),
        count: 2,
        retrying: true,
    });
    let changed = s
        .put_presence(o.id, &[failed.clone()], 90_000)
        .await
        .unwrap()
        .changed;
    assert_eq!(changed.len(), 1);
    let p = s.presence(o.id, Some("a")).await.unwrap();
    assert_eq!(p[0].turn_failed, failed.turn_failed);
    // A report without the failure clears it, also after the entry went
    // stale; a report that keeps it, or a seat that never failed, does not.
    let stored = s
        .put_presence(o.id, &[failed.clone()], 90_000)
        .await
        .unwrap();
    assert!(stored.turn_failures_cleared.is_empty());
    let stored = s
        .put_presence(o.id, &[rec("a", SeatState::Busy, now)], -1)
        .await
        .unwrap();
    assert_eq!(stored.turn_failures_cleared, vec!["a".to_string()]);
    let stored = s
        .put_presence(o.id, &[rec("a", SeatState::Idle, now)], 90_000)
        .await
        .unwrap();
    assert!(stored.turn_failures_cleared.is_empty());
    // Deleted seats have no presence; the system seat never shows.
    s.delete_seat(o.id, "a").await.unwrap();
    assert!(s.presence(o.id, None).await.unwrap().is_empty());
}

async fn send_budget_exact(s: S) {
    let (o, _) = org(&s, "acme").await;
    let a = seat(&s, o.id, "a").await;
    let mut tasks = Vec::new();
    for _ in 0..40 {
        let s = s.clone();
        tasks.push(tokio::spawn(async move {
            s.take_send_token(a.id, 0, 10).await.unwrap()
        }));
    }
    let mut ok = 0;
    for t in tasks {
        match t.await.unwrap() {
            Ok(()) => ok += 1,
            Err(wait) => assert!(wait > 0),
        }
    }
    assert_eq!(ok, 10);
    s.refund_send_token(a.id, 10).await.unwrap();
    assert!(s.take_send_token(a.id, 0, 10).await.unwrap().is_ok());
    assert!(s.take_send_token(a.id, 0, 10).await.unwrap().is_err());
    // Refill: 600 per minute is one per 100 ms.
    let b = seat(&s, o.id, "b").await;
    assert!(s.take_send_token(b.id, 600, 1).await.unwrap().is_ok());
    let wait = s.take_send_token(b.id, 600, 1).await.unwrap().unwrap_err();
    assert!((1..=100).contains(&wait), "{wait}");
    tokio::time::sleep(std::time::Duration::from_millis(wait + 20)).await;
    assert!(s.take_send_token(b.id, 600, 1).await.unwrap().is_ok());
}

async fn ack_steps_taken_once(s: S) {
    let (o, user) = org(&s, "acme").await;
    let a = seat(&s, o.id, "a").await;
    let b = seat(&s, o.id, "b").await;
    let mut m = msg(o.id, user.id, "ack me", &[(&a, C::To), (&b, C::Cc)]);
    m.ack_required = true;
    m.ack_due = Some(now_ms() - 1);
    let sent = s.send(&m).await.unwrap();
    let due = s.due_acks(now_ms(), 10).await.unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!((due[0].message_id, due[0].stage), (sent.message_id, 0));
    let names: Vec<String> = s
        .laggards(sent.message_id)
        .await
        .unwrap()
        .into_iter()
        .map(|l| l.name)
        .collect();
    assert_eq!(names, vec!["a", "b"]);
    s.ack(sent.message_id, b.id).await.unwrap();
    assert_eq!(s.laggards(sent.message_id).await.unwrap().len(), 1);
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let s = s.clone();
        let id = sent.message_id;
        tasks.push(tokio::spawn(async move {
            s.advance_ack(id, 0, 1, Some(now_ms() + 60_000))
                .await
                .unwrap()
        }));
    }
    let mut won = 0;
    for t in tasks {
        won += t.await.unwrap() as i32;
    }
    assert_eq!(won, 1);
    assert!(s.due_acks(now_ms(), 10).await.unwrap().is_empty());
    assert!(s.advance_ack(sent.message_id, 1, 2, None).await.unwrap());
    assert!(!s.advance_ack(sent.message_id, 2, 3, None).await.unwrap());
}

async fn system_seat_hidden(s: S) {
    let (o, _) = org(&s, "acme").await;
    let sys = s.system_seat(o.id).await.unwrap();
    assert_eq!(sys.kind, SeatKind::System);
    assert_eq!(s.system_seat(o.id).await.unwrap().id, sys.id);
    // Concurrent first use creates one.
    let (o2, _) = org(&s, "beta").await;
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let s = s.clone();
        tasks.push(tokio::spawn(async move {
            s.system_seat(o2.id).await.unwrap().id
        }));
    }
    let ids: HashSet<i64> = futures_ids(tasks).await;
    assert_eq!(ids.len(), 1);
    assert!(
        s.seats(o.id)
            .await
            .unwrap()
            .iter()
            .all(|x| x.kind != SeatKind::System)
    );
    assert!(s.seat_by_name(o.id, "amail").await.unwrap().is_none());
    // A real seat may take the name.
    let real = seat(&s, o.id, "amail").await;
    assert_eq!(real.kind, SeatKind::Agent);
}

async fn futures_ids(tasks: Vec<tokio::task::JoinHandle<i64>>) -> HashSet<i64> {
    let mut out = HashSet::new();
    for t in tasks {
        out.insert(t.await.unwrap());
    }
    out
}

async fn focus_and_digest_pending(s: S) {
    let (o, user) = org(&s, "acme").await;
    let a = seat(&s, o.id, "a").await;
    let before = s.pending(o.id, Some(a.id)).await.unwrap()[0].last_seq;
    let bump = s.set_focus(a.id, Some(now_ms() + 60_000)).await.unwrap();
    assert_eq!(bump.seq, before + 1);
    let p = &s.pending(o.id, Some(a.id)).await.unwrap()[0];
    assert!(p.focus_until.is_some());
    assert_eq!(
        s.seat_by_name(o.id, "a")
            .await
            .unwrap()
            .unwrap()
            .focus_until,
        p.focus_until
    );
    s.set_focus(a.id, None).await.unwrap();
    assert!(
        s.pending(o.id, Some(a.id)).await.unwrap()[0]
            .focus_until
            .is_none()
    );
    let mut m = msg(o.id, user.id, "digest me", &[(&a, C::To)]);
    m.deliveries[0].mode = M::Digest;
    s.send(&m).await.unwrap();
    let p = &s.pending(o.id, Some(a.id)).await.unwrap()[0];
    assert_eq!((p.to, p.digest), (0, 1));
    assert!(p.digest_since.is_some());
    let mut f = filter(&[C::To, C::Cc, C::List]);
    assert!(s.claim(a.id, &f).await.unwrap().0.is_empty());
    f.digest_only = true;
    assert_eq!(s.claim(a.id, &f).await.unwrap().0.len(), 1);
    assert_eq!(s.pending(o.id, Some(a.id)).await.unwrap()[0].digest, 0);
}

/// One step of a search looks at a bounded amount of body text: with a
/// time limit of zero a request does exactly one step, which covers as many
/// messages as fit in `SEARCH_STEP_BYTES` (divided by the number of terms,
/// at least one message), and answers with a cursor. Following the cursor
/// finds every match.
async fn search_steps_are_bounded(s: S) {
    use amail_proto::limits::SEARCH_STEP_BYTES;
    let (o, user) = org(&s, "acme").await;
    let a = seat(&s, o.id, "a").await;
    let body_bytes = 256 * 1024;
    let n = 40;
    let mut ids = Vec::new();
    for i in 0..n {
        let mut m = msg(o.id, user.id, &format!("m{i}"), &[(&a, C::To)]);
        m.body = format!("{} needle{i} ", "x".repeat(body_bytes - 16));
        ids.push(s.send(&m).await.unwrap().message_id);
    }
    let per_step = (SEARCH_STEP_BYTES / body_bytes as i64) as usize;
    assert!(per_step >= 2 && per_step < n, "{per_step}");
    for terms in [vec!["needle"], vec!["needle", "xxx"]] {
        let per_step = per_step / terms.len();
        let mut q = SearchQuery {
            terms: terms.iter().map(|t| t.to_string()).collect(),
            before_id: None,
            limit: 100,
            scan: amail_proto::limits::MAX_SCAN_MESSAGES,
            time_limit: Some(std::time::Duration::ZERO),
        };
        let mut found = Vec::new();
        let mut requests = 0;
        loop {
            let r = s.search(o.id, &q).await.unwrap();
            requests += 1;
            assert!(r.hits.len() <= per_step, "{terms:?}: {} hits", r.hits.len());
            found.extend(r.hits.iter().map(|h| h.message.id));
            match r.next_before_id {
                Some(next) => q.before_id = Some(next),
                None => break,
            }
        }
        found.reverse();
        assert_eq!(found, ids, "{terms:?}");
        assert_eq!(requests, n.div_ceil(per_step), "{terms:?}");
    }
    // Without a time limit, one request looks at all of them.
    let r = s
        .search(
            o.id,
            &SearchQuery {
                terms: vec!["needle".into()],
                before_id: None,
                limit: 100,
                scan: amail_proto::limits::MAX_SCAN_MESSAGES,
                time_limit: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(r.hits.len(), n);
    assert!(r.next_before_id.is_none());
}

/// Grants toward an org count only with an active link; a send records its
/// sender as a participant for the other orgs that got it; ending the link
/// forgets the participants of both orgs; teams open their contacts.
async fn grants_and_thread_peers(s: S) {
    use amail_store::{GrantFilter, GrantTarget, TeamChange};
    let (a, _) = org(&s, "alpha").await;
    let (b, ub) = org(&s, "bravo").await;
    let x = seat(&s, a.id, "x").await;
    let y = seat(&s, b.id, "y").await;
    let toward_b = GrantFilter {
        grantor: None,
        audience: Some((b.id, "bravo".into())),
    };
    // The user seat's grant exists from the start, but not linked: none.
    assert!(s.grants(&toward_b).await.unwrap().is_empty());
    s.request_link(a.id, b.id, Some("hi")).await.unwrap();
    assert!(s.grants(&toward_b).await.unwrap().is_empty());
    s.accept_link(b.id, a.id).await.unwrap();
    let g = s.grants(&toward_b).await.unwrap();
    assert_eq!(g.len(), 1);
    assert!(matches!(&g[0].target, GrantTarget::Seat(c) if c.name == "owner"));
    // A named audience; a team with a contact.
    s.add_grant(a.id, Some(x.id), None, Some("charlie"), false)
        .await
        .unwrap();
    assert_eq!(s.grants(&toward_b).await.unwrap().len(), 1);
    s.update_seat(
        a.id,
        "x",
        &amail_store::SeatUpdate {
            group: Some(Some("core".into())),
            contact: Some(true),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let team = s
        .put_team(a.id, "core", &TeamChange::default())
        .await
        .unwrap();
    s.add_grant(a.id, None, Some(team.id), None, true)
        .await
        .unwrap();
    let g = s.grants(&toward_b).await.unwrap();
    assert_eq!(g.len(), 2);
    match &g[1].target {
        GrantTarget::Team { team, contacts } => {
            assert_eq!(team.name, "core");
            assert_eq!(contacts.len(), 1);
            assert_eq!(contacts[0].name, "x");
        }
        t => panic!("{t:?}"),
    }
    assert!(g[1].managed);
    // Participants: every seat of a message that crossed orgs.
    let internal = s
        .send(&msg(a.id, x.id, "i", &[(&ub_seat(&s, a.id).await, C::To)]))
        .await
        .unwrap();
    assert!(
        s.thread_parts(a.id, &[internal.thread_id])
            .await
            .unwrap()
            .is_empty()
    );
    let sent = s.send(&msg(a.id, x.id, "q", &[(&y, C::To)])).await.unwrap();
    let p = s.thread_parts(b.id, &[sent.thread_id]).await.unwrap();
    let names: Vec<(&str, &str)> = p
        .iter()
        .map(|x| (x.org_name.as_str(), x.card.name.as_str()))
        .collect();
    assert_eq!(names, vec![("alpha", "x"), ("bravo", "y")]);
    let mut r = msg(b.id, ub.id, "a", &[(&x, C::To)]);
    r.thread_id = Some(sent.thread_id);
    s.send(&r).await.unwrap();
    assert_eq!(
        s.thread_parts(a.id, &[sent.thread_id]).await.unwrap().len(),
        3
    );
    // A third org knows nothing of the thread.
    let (c3, _) = org(&s, "charlie").await;
    assert!(
        s.thread_parts(c3.id, &[sent.thread_id])
            .await
            .unwrap()
            .is_empty()
    );
    // Primary contacts: per link and per team.
    s.set_primary(a.id, b.id, None, Some(x.id)).await.unwrap();
    s.set_primary(a.id, b.id, Some(team.id), Some(x.id))
        .await
        .unwrap();
    let mut pr = s.primaries(a.id).await.unwrap();
    pr.sort();
    assert_eq!(pr, vec![(b.id, None, x.id), (b.id, Some(team.id), x.id)]);
    s.set_primary(a.id, b.id, Some(team.id), None)
        .await
        .unwrap();
    assert_eq!(s.primaries(a.id).await.unwrap().len(), 1);
    s.revoke_link(a.id, b.id).await.unwrap();
    assert!(s.grants(&toward_b).await.unwrap().is_empty());
    assert!(s.primaries(a.id).await.unwrap().is_empty());
    // Deleting the team removes its grant and the membership.
    assert!(s.delete_team(a.id, "core").await.unwrap());
    assert!(
        s.seat_by_name(a.id, "x")
            .await
            .unwrap()
            .unwrap()
            .team_id
            .is_none()
    );
}

/// The org's user seat.
async fn ub_seat(s: &S, org_id: i64) -> Seat {
    s.user_seat(org_id).await.unwrap().unwrap()
}
