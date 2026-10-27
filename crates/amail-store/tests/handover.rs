//! Seat handover store conformance (M9): the handover note and the
//! worklog. Every test runs against SQLite and Postgres (Postgres only
//! when `AMAIL_TEST_PG_URL` is set; otherwise it is skipped with a
//! notice).

use amail_proto::{
    HandoverFields, HandoverSource, MAX_HANDOVER_NOTE_CHARS, MAX_WORKLOG_TEXT_CHARS, TaskState,
    WorklogSource,
};
use amail_store::*;
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
    note_fields_and_cap,
    revisions_and_private,
    pointers_from_the_worklog,
    read_acknowledgement_and_requests,
    idle_note,
    worklog_queries,
    server_entries_take_session_and_task,
    storage_quota,
    retention,
    org_deletion,
);

struct Org1 {
    org: Org,
    user: Seat,
    a: Seat,
    b: Seat,
}

async fn setup(s: &S, name: &str) -> Org1 {
    let (org, user) = s
        .create_org(name, "owner", &format!("hash-{name}"))
        .await
        .unwrap();
    let mut seats = Vec::new();
    for n in ["a", "b"] {
        seats.push(
            s.create_seat(
                org.id,
                &NewSeat {
                    name: n.into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap(),
        );
    }
    let b = seats.pop().unwrap();
    let a = seats.pop().unwrap();
    Org1 { org, user, a, b }
}

fn full() -> HandoverFields {
    HandoverFields {
        state: "on track".into(),
        summary: "Writing the parser.".into(),
        next: "1. cargo test -p parser".into(),
        waiting_on: "nothing".into(),
        traps: "Do not touch lexer.rs".into(),
        verify: "cargo test prints ok".into(),
        pointers: String::new(),
    }
}

fn note(o: &Org1, seat: &Seat, fields: HandoverFields) -> NewNoteRec {
    NewNoteRec {
        org_id: o.org.id,
        seat_id: seat.id,
        fields,
        private: None,
        source: HandoverSource::Agent,
        session: None,
        written_by: None,
        quota: None,
    }
}

fn entry(seat: &Seat, session: &str, kind: &str, text: &str, task: Option<i64>) -> NewWorklogEntry {
    NewWorklogEntry {
        seat_id: seat.id,
        session: Some(session.into()),
        at: now_ms(),
        kind: kind.into(),
        text: text.into(),
        task,
        doc: None,
        source: WorklogSource::Node,
    }
}

fn q(o: &Org1, seat: &Seat) -> WorklogQuery {
    WorklogQuery {
        org_id: o.org.id,
        seat_id: seat.id,
        limit: 100,
        ..Default::default()
    }
}

fn code(e: StoreError) -> &'static str {
    match e {
        StoreError::Refused(c, _) => c,
        e => panic!("not a refusal: {e:?}"),
    }
}

async fn note_fields_and_cap(s: S) {
    let o = setup(&s, "alpha").await;
    let mut f = full();
    f.next = " ".into();
    f.traps = String::new();
    let e = s.write_handover_note(&note(&o, &o.a, f)).await.unwrap_err();
    let msg = e.to_string();
    assert_eq!(code(e), "note_fields_missing");
    assert!(msg.contains("Next, Traps"), "{msg}");
    // Too large: the count is in the message.
    let mut f = full();
    f.traps = "x".repeat(MAX_HANDOVER_NOTE_CHARS);
    let e = s.write_handover_note(&note(&o, &o.a, f)).await.unwrap_err();
    let msg = e.to_string();
    assert_eq!(code(e), "note_too_large");
    assert!(msg.contains("16384"), "{msg}");
    // Nothing was written: the first good note is revision 1.
    let n = s
        .write_handover_note(&note(&o, &o.a, full()))
        .await
        .unwrap();
    assert_eq!(n.rev, 1);
    assert_eq!(n.fields.state, "on track");
    assert_eq!(n.source, HandoverSource::Agent);
    // A compaction draft is not checked for fields.
    let mut d = note(
        &o,
        &o.a,
        HandoverFields {
            state: "draft: from compaction summary".into(),
            summary: "the harness's summary".into(),
            ..Default::default()
        },
    );
    d.source = HandoverSource::Compaction;
    let n = s.write_handover_note(&d).await.unwrap();
    assert_eq!((n.rev, n.source), (2, HandoverSource::Compaction));
    let org = s.org_by_id(o.org.id).await.unwrap().unwrap();
    assert!(org.task_bytes > 0);
}

async fn revisions_and_private(s: S) {
    let o = setup(&s, "alpha").await;
    assert_eq!(s.handover_note(o.org.id, o.a.id, None).await.unwrap(), None);
    assert_eq!(s.handover_seat(o.org.id, o.a.id).await.unwrap().note_rev, 0);
    for i in 1..=3 {
        let mut f = full();
        f.summary = format!("revision {i}");
        let mut n = note(&o, &o.a, f);
        if i == 2 {
            n.private = Some(true);
        }
        assert_eq!(s.write_handover_note(&n).await.unwrap().rev, i);
    }
    let cur = s
        .handover_note(o.org.id, o.a.id, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((cur.rev, cur.fields.summary.as_str()), (3, "revision 3"));
    // `private` not given: as the previous note.
    assert!(cur.private);
    let r1 = s
        .handover_note(o.org.id, o.a.id, Some(1))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(r1.fields.summary, "revision 1");
    assert!(!r1.private);
    assert_eq!(
        s.handover_note(o.org.id, o.a.id, Some(9)).await.unwrap(),
        None
    );
    let revs: Vec<i64> = s
        .handover_notes(o.org.id, o.a.id, 2)
        .await
        .unwrap()
        .iter()
        .map(|n| n.rev)
        .collect();
    assert_eq!(revs, vec![3, 2]);
    // Seat b's notes are numbered on their own; another org sees none.
    assert_eq!(
        s.write_handover_note(&note(&o, &o.b, full()))
            .await
            .unwrap()
            .rev,
        1
    );
    let other = setup(&s, "beta").await;
    assert_eq!(
        s.handover_note(other.org.id, o.a.id, None).await.unwrap(),
        None
    );
    assert!(matches!(
        s.write_handover_note(&NewNoteRec {
            org_id: other.org.id,
            ..note(&o, &o.a, full())
        })
        .await
        .unwrap_err(),
        StoreError::NotFound(_)
    ));
    // The team file's setting.
    assert!(!s.handover_seat(o.org.id, o.b.id).await.unwrap().private);
    s.set_handover_private(o.org.id, o.b.id, true)
        .await
        .unwrap();
    assert!(s.handover_seat(o.org.id, o.b.id).await.unwrap().private);
    // Concurrent writers get distinct revisions.
    let mut hs = Vec::new();
    for _ in 0..8 {
        let s = s.clone();
        let n = note(&o, &o.b, full());
        hs.push(tokio::spawn(async move {
            s.write_handover_note(&n).await.unwrap().rev
        }));
    }
    let mut revs = Vec::new();
    for h in hs {
        revs.push(h.await.unwrap());
    }
    revs.sort_unstable();
    assert_eq!(revs, (2..=9).collect::<Vec<_>>());
}

async fn pointers_from_the_worklog(s: S) {
    let o = setup(&s, "alpha").await;
    s.append_worklog(
        o.org.id,
        &[
            entry(&o.a, "s1", "tool", "Edit src/a.rs", Some(4)),
            entry(&o.a, "s1", "commit", "abc1234 Fix the parser", None),
            NewWorklogEntry {
                doc: Some(3),
                ..entry(&o.a, "s1", "doc", "read d3", Some(7))
            },
            entry(&o.a, "s0", "tool", "old session", Some(99)),
        ],
        None,
    )
    .await
    .unwrap();
    let n = s
        .write_handover_note(&NewNoteRec {
            session: Some("s1".into()),
            ..note(&o, &o.a, full())
        })
        .await
        .unwrap();
    assert!(n.pointers_filled);
    let p = &n.fields.pointers;
    assert!(p.contains("Tasks: t7, t4"), "{p}");
    assert!(p.contains("Last commit: abc1234 Fix the parser"), "{p}");
    assert!(p.contains("Documents: d3"), "{p}");
    assert!(!p.contains("t99"), "{p}");
    // Given pointers are kept.
    let mut f = full();
    f.pointers = "t1, m5".into();
    let n = s.write_handover_note(&note(&o, &o.a, f)).await.unwrap();
    assert!(!n.pointers_filled);
    assert_eq!(n.fields.pointers, "t1, m5");
    // Without a session: the entries since the previous note.
    s.append_worklog(
        o.org.id,
        &[entry(&o.a, "s2", "tool", "later", Some(12))],
        None,
    )
    .await
    .unwrap();
    let n = s
        .write_handover_note(&note(&o, &o.a, full()))
        .await
        .unwrap();
    assert!(
        n.fields.pointers.contains("Tasks: t12\n"),
        "{}",
        n.fields.pointers
    );
    // Nothing recorded.
    let n = s
        .write_handover_note(&note(&o, &o.b, full()))
        .await
        .unwrap();
    assert!(n.fields.pointers.starts_with("nothing recorded"));
    // Filled pointers are cut to the cap.
    let mut f = full();
    f.traps = "x".repeat(MAX_HANDOVER_NOTE_CHARS - 200);
    let n = s.write_handover_note(&note(&o, &o.a, f)).await.unwrap();
    assert!(n.fields.render().chars().count() <= MAX_HANDOVER_NOTE_CHARS);
}

async fn read_acknowledgement_and_requests(s: S) {
    let o = setup(&s, "alpha").await;
    assert_eq!(
        s.record_handover_read(o.org.id, o.a.id, None, "s9")
            .await
            .unwrap(),
        None
    );
    s.write_handover_note(&note(&o, &o.a, full()))
        .await
        .unwrap();
    s.write_handover_note(&note(&o, &o.a, full()))
        .await
        .unwrap();
    let r = s
        .record_handover_read(o.org.id, o.a.id, None, "s9")
        .await
        .unwrap()
        .unwrap();
    assert_eq!((r.rev, r.read_by_session.as_deref()), (2, Some("s9")));
    assert!(r.read_at.is_some());
    let r1 = s
        .record_handover_read(o.org.id, o.a.id, Some(1), "s8")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(r1.read_by_session.as_deref(), Some("s8"));
    let st = s
        .request_handover(o.org.id, o.a.id, o.user.id, 1234)
        .await
        .unwrap();
    assert_eq!(st.requested_at, Some(1234));
    assert_eq!(st.requested_by.as_deref(), Some("owner"));
    assert_eq!(st.note_rev, 2);
}

async fn idle_note(s: S) {
    let o = setup(&s, "alpha").await;
    let mk = |title: &str| NewTaskRec {
        org_id: o.org.id,
        reporter_id: o.user.id,
        title: title.into(),
        assignee_id: Some(o.a.id),
        ..Default::default()
    };
    let t1 = s.create_task(&mk("parser")).await.unwrap();
    let t2 = s.create_task(&mk("lexer")).await.unwrap();
    let close = |num: i64| {
        let s = s.clone();
        let (org, a) = (o.org.id, o.a.id);
        async move {
            s.change_task(
                &TaskWrite {
                    org_id: org,
                    num,
                    actor_id: Some(a),
                    ..Default::default()
                },
                &TaskChange {
                    state: Some(TaskState::Done),
                    result: Some(Some(amail_proto::TaskResult::Ok)),
                    result_note: Some("all tests pass".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        }
    };
    close(t1.num).await;
    // t2 is still open: no note.
    assert_eq!(
        s.write_idle_note(o.org.id, o.a.id, t1.num, None)
            .await
            .unwrap(),
        None
    );
    close(t2.num).await;
    let n = s
        .write_idle_note(o.org.id, o.a.id, t2.num, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(n.source, HandoverSource::Server);
    assert!(n.fields.state.starts_with("idle"), "{}", n.fields.state);
    assert!(
        n.fields
            .summary
            .contains("t2 (lexer) is done with result ok")
    );
    assert!(n.fields.summary.contains("all tests pass"));
    assert!(
        n.fields.pointers.contains("Tasks: t2"),
        "{}",
        n.fields.pointers
    );
    assert!(n.fields.missing().is_empty());
}

async fn worklog_queries(s: S) {
    let o = setup(&s, "alpha").await;
    let base = now_ms() - 10_000;
    let mut es = Vec::new();
    for i in 0..30i64 {
        es.push(NewWorklogEntry {
            at: base + i * 100,
            ..entry(
                &o.a,
                "s1",
                if i % 3 == 0 { "commit" } else { "tool" },
                &format!("entry {i}"),
                (i % 2 == 0).then_some(i % 4 + 1),
            )
        });
    }
    // A long text is cut; an entry of another org's seat is left out.
    es.push(entry(&o.a, "s1", "tool", &"y".repeat(5000), None));
    let other = setup(&s, "beta").await;
    es.push(entry(&other.a, "s1", "tool", "foreign", None));
    // The user seat has no worklog.
    es.push(entry(&o.user, "s1", "tool", "user", None));
    assert_eq!(s.append_worklog(o.org.id, &es, None).await.unwrap(), 31);
    let all = s.worklog(&q(&o, &o.a)).await.unwrap();
    assert_eq!(all.len(), 31);
    assert_eq!(all[0].text.chars().count(), MAX_WORKLOG_TEXT_CHARS);
    assert!(all.windows(2).all(|w| w[0].id > w[1].id));
    assert_eq!(all[0].seat, "a");
    assert!(s.worklog(&q(&other, &other.a)).await.unwrap().is_empty());
    // Filters.
    let since = s
        .worklog(&WorklogQuery {
            since: Some(base + 2_500),
            ..q(&o, &o.a)
        })
        .await
        .unwrap();
    assert_eq!(since.len(), 5 + 1);
    let t1 = s
        .worklog(&WorklogQuery {
            task: Some(1),
            ..q(&o, &o.a)
        })
        .await
        .unwrap();
    assert!(!t1.is_empty() && t1.iter().all(|e| e.task == Some(1)));
    let commits = s
        .worklog(&WorklogQuery {
            kind: Some("commit".into()),
            limit: 3,
            ..q(&o, &o.a)
        })
        .await
        .unwrap();
    assert_eq!(commits.len(), 3);
    assert!(commits.iter().all(|e| e.kind == "commit"));
    let page2 = s
        .worklog(&WorklogQuery {
            kind: Some("commit".into()),
            before: Some(commits[2].id),
            ..q(&o, &o.a)
        })
        .await
        .unwrap();
    assert_eq!(page2.len(), 7);
    // The summary.
    let sum = s.worklog_summary(&q(&o, &o.a)).await.unwrap();
    assert_eq!(sum.total, 31);
    assert_eq!(
        sum.kinds,
        vec![("tool".to_string(), 21), ("commit".to_string(), 10)]
    );
    let mut tasks: Vec<i64> = sum.tasks.iter().map(|t| t.0).collect();
    tasks.sort_unstable();
    assert_eq!(tasks, vec![1, 3]);
    assert_eq!(sum.last.len(), 10);
    let sum = s
        .worklog_summary(&WorklogQuery {
            task: Some(3),
            ..q(&o, &o.a)
        })
        .await
        .unwrap();
    assert_eq!(sum.tasks.len(), 1);
}

async fn server_entries_take_session_and_task(s: S) {
    let o = setup(&s, "alpha").await;
    let t = s
        .create_task(&NewTaskRec {
            org_id: o.org.id,
            reporter_id: o.user.id,
            title: "x".into(),
            assignee_id: Some(o.a.id),
            ..Default::default()
        })
        .await
        .unwrap();
    s.change_task(
        &TaskWrite {
            org_id: o.org.id,
            num: t.num,
            actor_id: Some(o.a.id),
            ..Default::default()
        },
        &TaskChange {
            state: Some(TaskState::Doing),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    s.append_worklog(
        o.org.id,
        &[entry(&o.a, "sess-7", "session_start", "startup", None)],
        None,
    )
    .await
    .unwrap();
    s.append_worklog(
        o.org.id,
        &[NewWorklogEntry {
            session: None,
            source: WorklogSource::Server,
            ..entry(&o.a, "", "mail_sent", "sent m1", None)
        }],
        None,
    )
    .await
    .unwrap();
    let e = &s.worklog(&q(&o, &o.a)).await.unwrap()[0];
    assert_eq!(e.source, WorklogSource::Server);
    assert_eq!(e.session.as_deref(), Some("sess-7"));
    assert_eq!(e.task, Some(t.num));
    // The node's own entries keep what they say.
    assert_eq!(s.worklog(&q(&o, &o.a)).await.unwrap()[1].task, Some(t.num));
}

async fn storage_quota(s: S) {
    let o = setup(&s, "alpha").await;
    let quota = Some(20_000);
    for i in 0..30 {
        s.append_worklog(
            o.org.id,
            &[entry(
                &o.a,
                "s",
                "tool",
                &format!("{i} {}", "z".repeat(900)),
                None,
            )],
            quota,
        )
        .await
        .unwrap();
    }
    // The oldest entries made room for the newest.
    let all = s.worklog(&q(&o, &o.a)).await.unwrap();
    assert!(all.len() < 30 && all.len() > 10, "{}", all.len());
    assert!(all[0].text.starts_with("29 "));
    let org = s.org_by_id(o.org.id).await.unwrap().unwrap();
    assert!(org.stored_bytes + org.task_bytes <= 20_000);
    // A note over the quota is refused.
    let mut f = full();
    f.traps = "t".repeat(16_000);
    let e = s
        .write_handover_note(&NewNoteRec {
            quota: Some(org.stored_bytes + org.task_bytes + 100),
            ..note(&o, &o.a, f)
        })
        .await
        .unwrap_err();
    assert!(matches!(e, StoreError::StorageQuota(_)), "{e:?}");
}

async fn retention(s: S) {
    let o = setup(&s, "alpha").await;
    let now = now_ms();
    let mut es = Vec::new();
    for i in 0..10 {
        es.push(NewWorklogEntry {
            at: if i < 6 { now - 100 * 86_400_000 } else { now },
            ..entry(&o.a, "s", "tool", "x", None)
        });
    }
    s.append_worklog(o.org.id, &es, None).await.unwrap();
    let before = s.org_by_id(o.org.id).await.unwrap().unwrap().task_bytes;
    assert_eq!(s.expire_worklog(now - 90 * 86_400_000, 4).await.unwrap(), 4);
    assert_eq!(
        s.expire_worklog(now - 90 * 86_400_000, 100).await.unwrap(),
        2
    );
    assert_eq!(s.worklog(&q(&o, &o.a)).await.unwrap().len(), 4);
    let after = s.org_by_id(o.org.id).await.unwrap().unwrap().task_bytes;
    assert!(after < before && after > 0);
}

async fn org_deletion(s: S) {
    let o = setup(&s, "alpha").await;
    s.append_worklog(o.org.id, &[entry(&o.a, "s", "tool", "x", None)], None)
        .await
        .unwrap();
    s.write_handover_note(&note(&o, &o.a, full()))
        .await
        .unwrap();
    s.request_handover(o.org.id, o.a.id, o.user.id, 1)
        .await
        .unwrap();
    assert!(s.delete_org(o.org.id).await.unwrap());
    let o2 = setup(&s, "alpha").await;
    assert_eq!(
        s.handover_note(o2.org.id, o2.a.id, None).await.unwrap(),
        None
    );
    assert!(s.worklog(&q(&o2, &o2.a)).await.unwrap().is_empty());
}
