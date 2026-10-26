//! Task store conformance (M8). Every test runs against SQLite and Postgres
//! (Postgres only when `AMAIL_TEST_PG_URL` is set; otherwise it is skipped
//! with a notice).

use amail_proto::{TaskEventKind as K, TaskPriority, TaskState as St};
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
    create_and_number,
    transitions,
    dependencies_and_ready,
    cycles_and_bounds,
    take_race,
    links,
    events_for_every_change,
    events_are_kept,
    parents,
    queries,
    other_orgs_see_nothing,
    storage_quota,
    reminders_claimed_once,
    org_and_team_deletion,
    steal_requests,
    rev_check,
    idempotent_create,
    children_and_progress,
    results,
    needs_ok_readiness,
    import_twice,
    report,
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

async fn new(s: &S, o: &Org1, title: &str, assignee: Option<&Seat>) -> TaskRec {
    s.create_task(&NewTaskRec {
        org_id: o.org.id,
        reporter_id: o.user.id,
        title: title.into(),
        assignee_id: assignee.map(|x| x.id),
        ..Default::default()
    })
    .await
    .unwrap()
}

fn w(o: &Org1, num: i64, actor: &Seat) -> TaskWrite {
    TaskWrite {
        org_id: o.org.id,
        num,
        actor_id: Some(actor.id),
        ..Default::default()
    }
}

async fn set_state(s: &S, o: &Org1, num: i64, st: St) -> TaskChanged {
    s.change_task(
        &w(o, num, &o.user),
        &TaskChange {
            state: Some(st),
            ..Default::default()
        },
    )
    .await
    .unwrap()
}

async fn events(s: &S, o: &Org1, num: i64) -> Vec<TaskEventRec> {
    s.task_events(o.org.id, num, 0, 10_000)
        .await
        .unwrap()
        .unwrap()
        .0
}

fn code(e: StoreError) -> &'static str {
    match e {
        StoreError::Refused(c, _) => c,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

async fn create_and_number(s: S) {
    let o = setup(&s, "alpha").await;
    let t1 = new(&s, &o, "first", None).await;
    let t2 = new(&s, &o, "second", Some(&o.a)).await;
    assert_eq!((t1.num, t2.num), (1, 2));
    assert!(t2.rank > t1.rank, "new tasks go last");
    assert_eq!(t1.state, St::Open);
    assert!(t1.ready());
    assert_eq!(t1.shown_state(), St::Ready);
    assert_eq!(t1.priority, TaskPriority::Normal);
    assert_eq!(t2.assignee.as_deref(), Some("a"));
    assert_eq!(t2.reporter, "owner");
    let got = s.task(o.org.id, 2).await.unwrap().unwrap();
    assert_eq!(got, t2);
    assert!(s.task(o.org.id, 3).await.unwrap().is_none());
    let ev = events(&s, &o, 2).await;
    assert_eq!(
        ev.iter().map(|e| e.kind).collect::<Vec<_>>(),
        vec![K::Created, K::Assigned]
    );
    assert_eq!(ev[0].actor.as_deref(), Some("owner"));
    // Team defaults to the assignee's team, else the reporter's.
    s.update_seat(
        o.org.id,
        "a",
        &SeatUpdate {
            group: Some(Some("core".into())),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let t3 = new(&s, &o, "third", Some(&o.a)).await;
    assert_eq!(t3.team.as_deref(), Some("core"));
    let t4 = new(&s, &o, "fourth", Some(&o.b)).await;
    assert_eq!(t4.team, None);
}

async fn transitions(s: S) {
    let o = setup(&s, "alpha").await;
    let t = new(&s, &o, "x", Some(&o.a)).await;
    for st in [St::Doing, St::Review, St::Done] {
        let c = set_state(&s, &o, t.num, st).await;
        assert_eq!(c.task.state, st);
        assert_eq!(c.fields, vec!["state"]);
    }
    let done = s.task(o.org.id, t.num).await.unwrap().unwrap();
    assert!(done.closed_at.is_some());
    // A closed task is only reopened.
    for st in [St::Cancelled, St::Review, St::Blocked] {
        let e = s
            .change_task(
                &w(&o, t.num, &o.user),
                &TaskChange {
                    state: Some(st),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert_eq!(code(e), "invalid_transition");
    }
    let c = set_state(&s, &o, t.num, St::Open).await;
    assert_eq!(c.task.closed_at, None);
    // `ready` is never set.
    let e = s
        .change_task(
            &w(&o, t.num, &o.user),
            &TaskChange {
                state: Some(St::Ready),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(e, StoreError::Invalid(_)), "{e:?}");
    // Blocked with a seat and a reason; they go when it leaves blocked.
    let e = s
        .change_task(
            &w(&o, t.num, &o.a),
            &TaskChange {
                waiting_on_id: Some(Some(o.b.id)),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(code(e), "not_blocked");
    let c = s
        .change_task(
            &w(&o, t.num, &o.a),
            &TaskChange {
                state: Some(St::Blocked),
                waiting_on_id: Some(Some(o.b.id)),
                blocked_reason: Some(Some("needs a decision".into())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(c.task.waiting_on.as_deref(), Some("b"));
    assert_eq!(
        c.fields,
        vec!["state", "waiting_on", "blocked_reason"],
        "one event per field"
    );
    let c = set_state(&s, &o, t.num, St::Doing).await;
    assert_eq!(c.task.waiting_on, None);
    assert_eq!(c.task.blocked_reason, None);
    // Cancelled from any open state.
    set_state(&s, &o, t.num, St::Cancelled).await;
    // Nothing changed: no event, no new updated_at.
    let before = events(&s, &o, t.num).await.len();
    let c = set_state(&s, &o, t.num, St::Cancelled).await;
    assert!(c.fields.is_empty());
    assert_eq!(events(&s, &o, t.num).await.len(), before);
}

async fn dependencies_and_ready(s: S) {
    let o = setup(&s, "alpha").await;
    let t1 = new(&s, &o, "base", Some(&o.a)).await;
    let t2 = new(&s, &o, "other base", Some(&o.a)).await;
    let t3 = new(&s, &o, "top", Some(&o.b)).await;
    s.add_task_dep(&w(&o, t3.num, &o.user), t1.num)
        .await
        .unwrap();
    s.add_task_dep(&w(&o, t3.num, &o.user), t2.num)
        .await
        .unwrap();
    // Adding again changes nothing.
    let again = s
        .add_task_dep(&w(&o, t3.num, &o.user), t2.num)
        .await
        .unwrap();
    assert!(again.fields.is_empty());
    let top = s.task(o.org.id, t3.num).await.unwrap().unwrap();
    assert_eq!(top.waits_on, vec![1, 2]);
    assert_eq!(top.shown_state(), St::Open);
    // The first blocker closes: still waiting.
    let c = set_state(&s, &o, t1.num, St::Done).await;
    assert!(c.unblocked.is_empty());
    assert_eq!(c.dependents.len(), 1);
    let top = s.task(o.org.id, t3.num).await.unwrap().unwrap();
    assert_eq!(top.waits_on, vec![2]);
    // The last one is cancelled: ready, with an `unblocked` event.
    let c = set_state(&s, &o, t2.num, St::Cancelled).await;
    assert_eq!(c.unblocked.iter().map(|t| t.num).collect::<Vec<_>>(), [3]);
    let top = s.task(o.org.id, t3.num).await.unwrap().unwrap();
    assert!(top.ready());
    let ev = events(&s, &o, t3.num).await;
    let last = ev.last().unwrap();
    assert_eq!(last.kind, K::Unblocked);
    assert_eq!(last.note.as_deref(), Some("t2 cancelled"));
    // Reopening a blocker makes it wait again, without an event.
    let n = events(&s, &o, t3.num).await.len();
    set_state(&s, &o, t1.num, St::Open).await;
    let top = s.task(o.org.id, t3.num).await.unwrap().unwrap();
    assert_eq!(top.waits_on, vec![1]);
    assert_eq!(events(&s, &o, t3.num).await.len(), n);
    // Removing the last open dependency: unblocked.
    let c = s
        .remove_task_dep(&w(&o, t3.num, &o.user), t1.num)
        .await
        .unwrap();
    assert_eq!(c.unblocked.len(), 1);
    assert!(c.task.ready());
    assert_eq!(
        events(&s, &o, t3.num).await.last().unwrap().kind,
        K::Unblocked
    );
    let e = s
        .remove_task_dep(&w(&o, t3.num, &o.user), t1.num)
        .await
        .unwrap_err();
    assert!(matches!(e, StoreError::NotFound(_)));
    // Both tasks of an edge record it.
    let ev = events(&s, &o, t2.num).await;
    assert!(
        ev.iter()
            .any(|e| e.kind == K::DependencyAdded && e.to.as_deref() == Some("t3"))
    );
}

async fn cycles_and_bounds(s: S) {
    let o = setup(&s, "alpha").await;
    for i in 0..5 {
        new(&s, &o, &format!("t{}", i + 1), None).await;
    }
    // t1 <- t2 <- t3 (t3 depends on t2, t2 on t1).
    s.add_task_dep(&w(&o, 2, &o.a), 1).await.unwrap();
    s.add_task_dep(&w(&o, 3, &o.a), 2).await.unwrap();
    let e = s.add_task_dep(&w(&o, 1, &o.a), 3).await.unwrap_err();
    assert_eq!(code(e), "dependency_cycle");
    let e = s.add_task_dep(&w(&o, 1, &o.a), 1).await.unwrap_err();
    assert_eq!(code(e), "dependency_cycle");
    let e = s.add_task_dep(&w(&o, 1, &o.a), 99).await.unwrap_err();
    assert!(matches!(e, StoreError::NotFound(_)));
    // Depth: a chain longer than the check follows is refused.
    let mut prev = 5;
    let mut refused = None;
    for i in 0..40 {
        let t = new(&s, &o, &format!("chain {i}"), None).await;
        match s.add_task_dep(&w(&o, t.num, &o.a), prev).await {
            Ok(_) => prev = t.num,
            Err(e) => {
                refused = Some(code(e));
                break;
            }
        }
    }
    assert_eq!(refused, Some("dependency_too_deep"));
    // Fan-out: blockers per task.
    let hub = new(&s, &o, "hub", None).await;
    let mut result = Ok(());
    for i in 0..=amail_proto::limits::MAX_TASK_BLOCKERS {
        let t = new(&s, &o, &format!("dep {i}"), None).await;
        if let Err(e) = s.add_task_dep(&w(&o, hub.num, &o.a), t.num).await {
            result = Err(code(e));
            break;
        }
    }
    assert_eq!(result, Err("too_many_blockers"));
}

async fn take_race(s: S) {
    let o = setup(&s, "alpha").await;
    let t = new(&s, &o, "pool task", None).await;
    let mut seats = Vec::new();
    for i in 0..8 {
        seats.push(
            s.create_seat(
                o.org.id,
                &NewSeat {
                    name: format!("w{i}"),
                    ..Default::default()
                },
            )
            .await
            .unwrap(),
        );
    }
    let mut handles = Vec::new();
    for seat in &seats {
        let (s, ww) = (s.clone(), w(&o, t.num, seat));
        handles.push(tokio::spawn(async move { s.take_task(&ww).await }));
    }
    let mut won = Vec::new();
    for h in handles {
        match h.await.unwrap() {
            Ok(c) => won.push(c.task.assignee.unwrap()),
            Err(e) => assert_eq!(code(e), "taken"),
        }
    }
    assert_eq!(won.len(), 1, "{won:?}");
    let got = s.task(o.org.id, t.num).await.unwrap().unwrap();
    assert_eq!(got.assignee.as_ref(), Some(&won[0]));
    // Taking one's own task again changes nothing.
    let winner = seats.iter().find(|x| x.name == won[0]).unwrap();
    let c = s.take_task(&w(&o, t.num, winner)).await.unwrap();
    assert!(c.fields.is_empty());
    let assigned = events(&s, &o, t.num)
        .await
        .iter()
        .filter(|e| e.kind == K::Assigned)
        .count();
    assert_eq!(assigned, 1);
    // A closed task is not taken.
    let t2 = new(&s, &o, "closed", None).await;
    set_state(&s, &o, t2.num, St::Done).await;
    let e = s.take_task(&w(&o, t2.num, &o.a)).await.unwrap_err();
    assert_eq!(code(e), "closed");
}

async fn links(s: S) {
    let o = setup(&s, "alpha").await;
    let t = new(&s, &o, "x", None).await;
    assert!(
        s.add_task_link(&w(&o, t.num, &o.a), 100, false, None)
            .await
            .unwrap()
    );
    assert!(
        !s.add_task_link(&w(&o, t.num, &o.a), 100, false, None)
            .await
            .unwrap()
    );
    // Auto links: no error for a missing task.
    assert!(
        !s.add_task_link(&w(&o, 77, &o.a), 100, true, None)
            .await
            .unwrap()
    );
    let d = s.task_detail(o.org.id, t.num, 20).await.unwrap().unwrap();
    assert_eq!(d.links.len(), 1);
    assert_eq!(d.links[0].thread_id, 100);
    assert_eq!(d.links[0].seat.as_deref(), Some("a"));
    assert_eq!(
        s.thread_tasks(o.org.id, 100)
            .await
            .unwrap()
            .iter()
            .map(|t| t.num)
            .collect::<Vec<_>>(),
        [1]
    );
    assert!(s.remove_task_link(&w(&o, t.num, &o.a), 100).await.unwrap());
    assert!(!s.remove_task_link(&w(&o, t.num, &o.a), 100).await.unwrap());
    // Bound.
    for th in 0..amail_proto::limits::MAX_TASK_LINKS {
        s.add_task_link(&w(&o, t.num, &o.a), 1000 + th, false, None)
            .await
            .unwrap();
    }
    let e = s
        .add_task_link(&w(&o, t.num, &o.a), 5000, false, None)
        .await
        .unwrap_err();
    assert_eq!(code(e), "too_many_links");
    assert!(
        !s.add_task_link(&w(&o, t.num, &o.a), 5000, true, None)
            .await
            .unwrap()
    );
    // The discussion thread is not linked automatically.
    let t2 = new(&s, &o, "y", None).await;
    s.record_task_comment(&w(&o, t2.num, &o.a), 9, 9)
        .await
        .unwrap();
    assert!(
        !s.add_task_link(&w(&o, t2.num, &o.a), 9, true, None)
            .await
            .unwrap()
    );
    let ev = events(&s, &o, t.num).await;
    assert!(ev.iter().any(|e| e.kind == K::Unlinked));
}

async fn events_for_every_change(s: S) {
    let o = setup(&s, "alpha").await;
    let t = new(&s, &o, "x", None).await;
    let c = s
        .change_task(
            &TaskWrite {
                written_by: Some("owner".into()),
                ..w(&o, t.num, &o.a)
            },
            &TaskChange {
                title: Some("y".into()),
                body: Some("long body".into()),
                priority: Some(TaskPriority::Urgent),
                rank: Some(5),
                assignee_id: Some(Some(o.b.id)),
                labels: Some(vec!["ui".into(), "bug".into()]),
                due: Some(Some(1_800_000_000_000)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        c.fields,
        vec![
            "title", "body", "priority", "rank", "assignee", "labels", "due"
        ]
    );
    let ev = events(&s, &o, t.num).await;
    let tail: Vec<_> = ev[1..].iter().collect();
    assert_eq!(tail.len(), 7);
    assert!(tail.iter().all(|e| e.actor.as_deref() == Some("a")));
    assert!(
        tail.iter()
            .all(|e| e.written_by.as_deref() == Some("owner"))
    );
    let title = tail[0];
    assert_eq!(
        (title.from.as_deref(), title.to.as_deref()),
        (Some("x"), Some("y"))
    );
    assert_eq!(tail[4].kind, K::Assigned);
    assert_eq!(tail[5].to.as_deref(), Some("ui,bug"));
    assert_eq!(tail[6].to.as_deref(), Some("2027-01-15T08:00:00Z"));
    // Comments and reminders are events too; seq goes up by one.
    s.record_task_comment(&w(&o, t.num, &o.b), 42, 42)
        .await
        .unwrap();
    let id = s.task(o.org.id, t.num).await.unwrap().unwrap().id;
    s.record_task_reminder(o.org.id, id, "assigned", "b", 43)
        .await
        .unwrap();
    let ev = events(&s, &o, t.num).await;
    let n = ev.len();
    assert_eq!(ev[n - 2].kind, K::Commented);
    assert_eq!(ev[n - 2].message_id, Some(42));
    assert_eq!(ev[n - 1].kind, K::ReminderSent);
    assert_eq!(ev[n - 1].actor, None);
    assert!(ev.windows(2).all(|p| p[1].seq == p[0].seq + 1));
    let task = s.task(o.org.id, t.num).await.unwrap().unwrap();
    assert_eq!(task.thread_id, Some(42));
    // Paging.
    let (page, _) = s.task_events(o.org.id, t.num, 3, 2).await.unwrap().unwrap();
    assert_eq!(page.iter().map(|e| e.seq).collect::<Vec<_>>(), [4, 5]);
}

async fn events_are_kept(s: S) {
    let o = setup(&s, "alpha").await;
    let t = new(&s, &o, "x", None).await;
    let n = 1_205;
    for i in 0..n {
        s.record_task_reminder(o.org.id, t.id, "stale", &format!("r{i}"), i)
            .await
            .unwrap();
    }
    let (ev, compacted) = s
        .task_events(o.org.id, t.num, 0, 10_000)
        .await
        .unwrap()
        .unwrap();
    // The `created` event and every reminder.
    assert_eq!(ev.len() as i64, n + 1);
    assert_eq!(compacted, 0);
    assert_eq!(ev[0].seq, 1);
    let d = s.task_detail(o.org.id, t.num, 20).await.unwrap().unwrap();
    assert_eq!(d.events.len(), 20);
    assert_eq!(d.events.last().unwrap().seq, n + 1);
    assert_eq!(d.task.event_seq, n + 1);
    // A change of the body records the whole old and new text.
    let body = "line one\nline two".to_string();
    s.change_task(
        &w(&o, t.num, &o.a),
        &TaskChange {
            body: Some(body.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let e = events(&s, &o, t.num).await.pop().unwrap();
    assert_eq!(e.field.as_deref(), Some("body"));
    assert_eq!(e.from.as_deref(), Some(""));
    assert_eq!(e.to.as_deref(), Some(body.as_str()));
}

async fn parents(s: S) {
    let o = setup(&s, "alpha").await;
    let mut prev: Option<i64> = None;
    // A top task and 8 levels below it.
    for i in 0..9 {
        let t = s
            .create_task(&NewTaskRec {
                org_id: o.org.id,
                reporter_id: o.a.id,
                title: format!("level {i}"),
                parent: prev,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(t.parent, prev);
        prev = Some(t.num);
    }
    // A ninth level below the top is refused.
    let e = s
        .create_task(&NewTaskRec {
            org_id: o.org.id,
            reporter_id: o.a.id,
            title: "too deep".into(),
            parent: prev,
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(code(e), "parent_too_deep");
    // Cycles are refused.
    let e = s
        .change_task(
            &w(&o, 1, &o.a),
            &TaskChange {
                parent: Some(Some(3)),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(code(e), "parent_cycle");
    // Moving a subtree (t2 has seven levels below it) under a task that
    // has a parent is refused too.
    let top = new(&s, &o, "top", None).await;
    let lone = s
        .create_task(&NewTaskRec {
            org_id: o.org.id,
            reporter_id: o.a.id,
            title: "lone".into(),
            parent: Some(top.num),
            ..Default::default()
        })
        .await
        .unwrap();
    let e = s
        .change_task(
            &w(&o, 2, &o.a),
            &TaskChange {
                parent: Some(Some(lone.num)),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(code(e), "parent_too_deep");
    let d = s.task_detail(o.org.id, 1, 20).await.unwrap().unwrap();
    assert_eq!(d.children.iter().map(|t| t.num).collect::<Vec<_>>(), [2]);
    let c = s
        .change_task(
            &w(&o, 5, &o.a),
            &TaskChange {
                parent: Some(None),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(c.task.parent, None);
}

async fn queries(s: S) {
    let o = setup(&s, "alpha").await;
    // a: t1 (ready, low), t2 (ready, urgent), t3 (waits on t5 of b),
    //    t4 (blocked waiting on b); b: t5 (doing); unassigned t6.
    let t1 = new(&s, &o, "one", Some(&o.a)).await;
    let t2 = new(&s, &o, "two", Some(&o.a)).await;
    let t3 = new(&s, &o, "three", Some(&o.a)).await;
    let t4 = new(&s, &o, "four", Some(&o.a)).await;
    let t5 = new(&s, &o, "five searchable", Some(&o.b)).await;
    let t6 = new(&s, &o, "six", None).await;
    let set = |num: i64, ch: TaskChange| {
        let (s, ww) = (s.clone(), w(&o, num, &o.user));
        async move { s.change_task(&ww, &ch).await.unwrap() }
    };
    set(
        t1.num,
        TaskChange {
            priority: Some(TaskPriority::Low),
            labels: Some(vec!["ui".into()]),
            ..Default::default()
        },
    )
    .await;
    set(
        t2.num,
        TaskChange {
            priority: Some(TaskPriority::Urgent),
            ..Default::default()
        },
    )
    .await;
    s.add_task_dep(&w(&o, t3.num, &o.user), t5.num)
        .await
        .unwrap();
    set(
        t4.num,
        TaskChange {
            state: Some(St::Blocked),
            waiting_on_id: Some(Some(o.b.id)),
            ..Default::default()
        },
    )
    .await;
    set(
        t5.num,
        TaskChange {
            state: Some(St::Doing),
            ..Default::default()
        },
    )
    .await;
    let q = |f: fn(&mut TaskQuery)| {
        let mut q = TaskQuery {
            org_id: o.org.id,
            limit: 50,
            ..Default::default()
        };
        f(&mut q);
        q
    };
    let nums = |p: TaskPage| p.tasks.iter().map(|t| t.num).collect::<Vec<_>>();
    // mine: ready first (urgent before low), then doing, blocked, open.
    let mut mine = q(|q| q.order = TaskOrder::Personal);
    mine.assignee_id = Some(Some(o.a.id));
    assert_eq!(nums(s.tasks(&mine).await.unwrap()), [2, 1, 4, 3]);
    // next: ready ones; with the pool, unassigned ready ones of the team.
    let mut next = mine.clone();
    next.states = vec![St::Ready];
    assert_eq!(nums(s.tasks(&next).await.unwrap()), [2, 1]);
    // Filters.
    assert_eq!(
        nums(s.tasks(&q(|q| q.states = vec![St::Open])).await.unwrap()),
        [3]
    );
    assert_eq!(
        nums(s.tasks(&q(|q| q.label = Some("ui".into()))).await.unwrap()),
        [1]
    );
    assert_eq!(
        nums(
            s.tasks(&q(|q| q.text = Some("SEARCHABLE".into())))
                .await
                .unwrap()
        ),
        [5]
    );
    assert_eq!(
        nums(s.tasks(&q(|q| q.assignee_id = Some(None))).await.unwrap()),
        [t6.num]
    );
    assert_eq!(
        nums(
            s.tasks(&q(|q| q.priority = Some(TaskPriority::Urgent)))
                .await
                .unwrap()
        ),
        [2]
    );
    let all = s.tasks(&q(|q| q.limit = 4)).await.unwrap();
    assert_eq!(nums(all.clone()), [6, 5, 4, 3]);
    assert_eq!(all.next_before, Some(3));
    let mut rest = q(|_| {});
    rest.before = all.next_before;
    assert_eq!(nums(s.tasks(&rest).await.unwrap()), [2, 1]);
    // Stale: doing, not changed since.
    let stale = q(|q| q.stale_before = Some(i64::MAX));
    assert_eq!(nums(s.tasks(&stale).await.unwrap()), [5]);
    // waiting (for a): t4 on b; t3 on t5 (b).
    let (wt, _) = s.tasks_waiting(o.org.id, o.a.id, 50).await.unwrap();
    let got: Vec<_> = wt
        .iter()
        .map(|x| (x.task.num, x.seat.clone(), x.other.as_ref().map(|t| t.num)))
        .collect();
    assert_eq!(
        got,
        [(4, Some("b".into()), None), (3, Some("b".into()), Some(5))]
    );
    // owed (by b): t4 waits on b; t5 is the last blocker of a's t3.
    let (ow, _) = s.tasks_owed(o.org.id, o.b.id, 50).await.unwrap();
    let got: Vec<_> = ow
        .iter()
        .map(|x| (x.task.num, x.seat.clone(), x.other.as_ref().map(|t| t.num)))
        .collect();
    assert_eq!(
        got,
        [(4, Some("a".into()), None), (5, Some("a".into()), Some(3))]
    );
    // Board.
    let b = s
        .task_board(o.org.id, None, 24, amail_store::now_ms())
        .await
        .unwrap();
    let count = |st: St| b.columns.iter().find(|c| c.0 == st).unwrap().1;
    assert_eq!(count(St::Ready), 3);
    assert_eq!(count(St::Open), 1);
    assert_eq!(count(St::Doing), 1);
    assert_eq!(count(St::Blocked), 1);
    let load_a = b.load.iter().find(|l| l.seat == "a").unwrap();
    assert_eq!((load_a.ready, load_a.waiting, load_a.blocked), (2, 1, 1));
    assert_eq!(b.chains.iter().map(|t| t.num).collect::<Vec<_>>(), [3]);
    assert!(b.stale.is_empty());
}

async fn other_orgs_see_nothing(s: S) {
    let a = setup(&s, "alpha").await;
    let b = setup(&s, "beta").await;
    let ta = new(&s, &a, "secret", None).await;
    let tb = new(&s, &b, "theirs", None).await;
    assert_eq!((ta.num, tb.num), (1, 1), "numbers are per org");
    assert_eq!(s.task(b.org.id, 1).await.unwrap().unwrap().title, "theirs");
    assert!(s.task(b.org.id, 2).await.unwrap().is_none());
    let page = s
        .tasks(&TaskQuery {
            org_id: b.org.id,
            limit: 50,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(page.tasks.len(), 1);
    // Writes name a number of the writer's org only.
    let e = s.add_task_dep(&w(&b, 1, &b.a), 2).await.unwrap_err();
    assert!(matches!(e, StoreError::NotFound(_)));
    s.add_task_link(&w(&a, 1, &a.a), 500, false, None)
        .await
        .unwrap();
    assert!(s.thread_tasks(b.org.id, 500).await.unwrap().is_empty());
}

async fn storage_quota(s: S) {
    let o = setup(&s, "alpha").await;
    let quota = Some(4096);
    let mk = |title: &str, body: usize| NewTaskRec {
        org_id: o.org.id,
        reporter_id: o.user.id,
        title: title.into(),
        body: "x".repeat(body),
        quota,
        ..Default::default()
    };
    s.create_task(&mk("small", 100)).await.unwrap();
    let e = s.create_task(&mk("big", 8000)).await.unwrap_err();
    assert!(matches!(e, StoreError::StorageQuota(_)), "{e:?}");
    // The refused task took no number.
    let t = s.create_task(&mk("next", 10)).await.unwrap();
    assert_eq!(t.num, 2);
    let org = s.org_by_id(o.org.id).await.unwrap().unwrap();
    assert!(org.task_bytes > 0);
    // Growing a body over the quota is refused; shrinking is not.
    let e = s
        .change_task(
            &TaskWrite {
                quota,
                ..w(&o, 1, &o.a)
            },
            &TaskChange {
                body: Some("y".repeat(8000)),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(e, StoreError::StorageQuota(_)));
    s.change_task(
        &TaskWrite {
            quota,
            ..w(&o, 1, &o.a)
        },
        &TaskChange {
            body: Some(String::new()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let after = s.org_by_id(o.org.id).await.unwrap().unwrap().task_bytes;
    assert!(after < org.task_bytes + 200, "{after} {}", org.task_bytes);
}

async fn reminders_claimed_once(s: S) {
    let o = setup(&s, "alpha").await;
    let t = new(&s, &o, "due", Some(&o.a)).await;
    s.change_task(
        &w(&o, t.num, &o.user),
        &TaskChange {
            due: Some(Some(1000)),
            state: Some(St::Doing),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let now = amail_store::now_ms();
    let (x, y) = tokio::join!(s.claim_due_tasks(now, 10), s.claim_due_tasks(now, 10));
    assert_eq!(x.unwrap().len() + y.unwrap().len(), 1);
    assert!(s.claim_due_tasks(now, 10).await.unwrap().is_empty());
    // A new due time is reminded again.
    s.change_task(
        &w(&o, t.num, &o.user),
        &TaskChange {
            due: Some(Some(2000)),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(s.claim_due_tasks(now, 10).await.unwrap().len(), 1);
    // Stale: one pass per org per hour, each task once per period.
    let later = now + 25 * 3_600_000;
    let (x, y) = tokio::join!(
        s.claim_stale_tasks(later, 10),
        s.claim_stale_tasks(later, 10)
    );
    assert_eq!(x.unwrap().len() + y.unwrap().len(), 1);
    assert!(
        s.claim_stale_tasks(later + 1_800_000, 10)
            .await
            .unwrap()
            .is_empty()
    );
    // An hour later the pass runs, but the task was reminded this period.
    assert!(
        s.claim_stale_tasks(later + 3_600_000, 10)
            .await
            .unwrap()
            .is_empty()
    );
    // A period later it is reminded again.
    assert_eq!(
        s.claim_stale_tasks(later + 26 * 3_600_000, 10)
            .await
            .unwrap()
            .len(),
        1
    );
    // Stale period 0: off.
    s.update_org(
        o.org.id,
        &OrgSettings {
            task_stale_hours: Some(0),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(
        s.claim_stale_tasks(later + 100 * 3_600_000, 10)
            .await
            .unwrap()
            .is_empty()
    );
}

async fn org_and_team_deletion(s: S) {
    let o = setup(&s, "alpha").await;
    s.update_seat(
        o.org.id,
        "a",
        &SeatUpdate {
            group: Some(Some("core".into())),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let t = new(&s, &o, "x", Some(&o.a)).await;
    assert_eq!(t.team.as_deref(), Some("core"));
    let t2 = new(&s, &o, "y", None).await;
    s.add_task_dep(&w(&o, t2.num, &o.a), t.num).await.unwrap();
    s.add_task_link(&w(&o, t.num, &o.a), 7, false, None)
        .await
        .unwrap();
    assert!(s.delete_team(o.org.id, "core").await.unwrap());
    assert_eq!(s.task(o.org.id, t.num).await.unwrap().unwrap().team, None);
    assert!(s.delete_org(o.org.id).await.unwrap());
    // The name can be taken again; the new org starts at t1.
    let o2 = setup(&s, "alpha").await;
    assert_eq!(new(&s, &o2, "fresh", None).await.num, 1);
}

async fn steal_requests(s: S) {
    let o = setup(&s, "alpha").await;
    let c = s
        .create_seat(
            o.org.id,
            &NewSeat {
                name: "c".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    // Unassigned: take it instead.
    let free = new(&s, &o, "free", None).await;
    let e = s.steal_task(&w(&o, free.num, &o.b)).await.unwrap_err();
    assert_eq!(code(e), "not_assigned");
    let t = new(&s, &o, "x", Some(&o.a)).await;
    let e = s.steal_task(&w(&o, t.num, &o.a)).await.unwrap_err();
    assert_eq!(code(e), "own_task");
    // A request; asking again changes nothing; a second seat waits.
    let ch = s.steal_task(&w(&o, t.num, &o.b)).await.unwrap();
    assert_eq!(ch.task.steal_requested_by.as_deref(), Some("b"));
    assert!(ch.task.steal_requested_at.is_some());
    assert!(
        s.steal_task(&w(&o, t.num, &o.b))
            .await
            .unwrap()
            .fields
            .is_empty()
    );
    let e = s.steal_task(&w(&o, t.num, &c)).await.unwrap_err();
    assert_eq!(code(e), "steal_pending");
    // Only the assignee (or the user) answers.
    let e = s
        .answer_task_steal(&w(&o, t.num, &c), true, false)
        .await
        .unwrap_err();
    assert_eq!(code(e), "not_assignee");
    // Keep: cleared.
    let ch = s
        .answer_task_steal(&w(&o, t.num, &o.a), false, false)
        .await
        .unwrap();
    assert_eq!(ch.task.steal_requested_by, None);
    assert_eq!(ch.task.assignee.as_deref(), Some("a"));
    let e = s
        .answer_task_steal(&w(&o, t.num, &o.a), false, false)
        .await
        .unwrap_err();
    assert!(matches!(e, StoreError::NotFound(_)));
    // Yield (by the user): the requester has it.
    s.steal_task(&w(&o, t.num, &c)).await.unwrap();
    let ch = s
        .answer_task_steal(&w(&o, t.num, &o.user), true, true)
        .await
        .unwrap();
    assert_eq!(ch.task.assignee.as_deref(), Some("c"));
    assert_eq!(ch.task.steal_requested_by, None);
    let ev = events(&s, &o, t.num).await;
    let kinds: Vec<K> = ev.iter().map(|e| e.kind).collect();
    assert!(kinds.contains(&K::StealRequested) && kinds.contains(&K::StealDeclined));
    let last = ev.last().unwrap();
    assert_eq!(last.kind, K::Assigned);
    assert_eq!(last.note.as_deref(), Some("yielded to c on request"));
    // Assigning by hand clears a request.
    s.steal_task(&w(&o, t.num, &o.b)).await.unwrap();
    let ch = s
        .change_task(
            &w(&o, t.num, &o.user),
            &TaskChange {
                assignee_id: Some(Some(o.a.id)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(ch.task.steal_requested_by, None);
    assert!(ch.fields.contains(&"steal".to_string()));
    let last = events(&s, &o, t.num).await.pop().unwrap();
    assert_eq!(last.kind, K::StealDeclined);
    assert!(last.note.unwrap().starts_with("cleared"));
    // So does starting it; a started task cannot be asked for.
    s.steal_task(&w(&o, t.num, &o.b)).await.unwrap();
    let ch = set_state(&s, &o, t.num, St::Doing).await;
    assert_eq!(ch.task.steal_requested_by, None);
    let e = s.steal_task(&w(&o, t.num, &o.b)).await.unwrap_err();
    assert_eq!(code(e), "started");
    // Expiry: each request once.
    set_state(&s, &o, t.num, St::Open).await;
    s.steal_task(&w(&o, t.num, &o.b)).await.unwrap();
    let now = amail_store::now_ms();
    assert!(
        s.expire_task_steals(now - 3_600_000, 10)
            .await
            .unwrap()
            .is_empty()
    );
    let (x, y) = tokio::join!(
        s.expire_task_steals(now + 1, 10),
        s.expire_task_steals(now + 1, 10)
    );
    let mut all = x.unwrap();
    all.extend(y.unwrap());
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].requester, "b");
    assert_eq!(all[0].task.steal_requested_by, None);
    assert_eq!(
        events(&s, &o, t.num).await.pop().unwrap().kind,
        K::StealExpired
    );
}

// --- M8.1 -------------------------------------------------------------------

async fn rev_check(s: S) {
    let o = setup(&s, "alpha").await;
    let t = new(&s, &o, "x", Some(&o.a)).await;
    // `created` and `assigned`.
    assert_eq!(t.event_seq, 2);
    let stale = TaskWrite {
        if_rev: Some(1),
        ..w(&o, t.num, &o.a)
    };
    let title = |v: &str| TaskChange {
        title: Some(v.into()),
        ..Default::default()
    };
    let e = s.change_task(&stale, &title("y")).await.unwrap_err();
    assert_eq!(code(e), "stale");
    // Nothing changed.
    assert_eq!(s.task(o.org.id, t.num).await.unwrap().unwrap().title, "x");
    let fresh = TaskWrite {
        if_rev: Some(2),
        ..w(&o, t.num, &o.a)
    };
    let c = s.change_task(&fresh, &title("y")).await.unwrap();
    assert_eq!(c.task.event_seq, 3);
    // The other single-task writes check it too.
    let other = new(&s, &o, "other", None).await;
    assert_eq!(
        code(s.set_task_dep(&stale, other.num, None).await.unwrap_err()),
        "stale"
    );
    assert_eq!(
        code(s.remove_task_dep(&stale, other.num).await.unwrap_err()),
        "stale"
    );
    assert_eq!(
        code(s.add_task_link(&stale, 99, false, None).await.unwrap_err()),
        "stale"
    );
    assert_eq!(
        code(s.remove_task_link(&stale, 99).await.unwrap_err()),
        "stale"
    );
    let take = TaskWrite {
        if_rev: Some(0),
        ..w(&o, other.num, &o.b)
    };
    assert_eq!(code(s.take_task(&take).await.unwrap_err()), "stale");
    // Without if_rev a write is not checked.
    s.change_task(&w(&o, t.num, &o.b), &title("z"))
        .await
        .unwrap();
}

async fn idempotent_create(s: S) {
    let o = setup(&s, "alpha").await;
    let mk = |key: Option<&str>, title: &str| NewTaskRec {
        org_id: o.org.id,
        reporter_id: o.a.id,
        title: title.into(),
        client_key: key.map(str::to_string),
        ..Default::default()
    };
    let (t1, existed) = s.create_task_once(&mk(Some("k1"), "first")).await.unwrap();
    assert!(!existed);
    let (again, existed) = s.create_task_once(&mk(Some("k1"), "second")).await.unwrap();
    assert!(existed);
    assert_eq!(again.num, t1.num);
    assert_eq!(again.title, "first");
    // Another key, or none, creates a task.
    let (t2, existed) = s.create_task_once(&mk(Some("k2"), "first")).await.unwrap();
    assert!(!existed && t2.num == t1.num + 1);
    let (t3, existed) = s.create_task_once(&mk(None, "first")).await.unwrap();
    assert!(!existed && t3.num == t2.num + 1);
    // Keys are per org.
    let other = setup(&s, "beta").await;
    let (t, existed) = s
        .create_task_once(&NewTaskRec {
            org_id: other.org.id,
            reporter_id: other.a.id,
            title: "first".into(),
            client_key: Some("k1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(!existed && t.num == 1);
}

async fn children_and_progress(s: S) {
    let o = setup(&s, "alpha").await;
    let parent = new(&s, &o, "parent", Some(&o.a)).await;
    let mut kids = Vec::new();
    for i in 0..4 {
        kids.push(
            s.create_task(&NewTaskRec {
                org_id: o.org.id,
                reporter_id: o.user.id,
                title: format!("child {i}"),
                parent: Some(parent.num),
                ..Default::default()
            })
            .await
            .unwrap(),
        );
    }
    let p = s.task(o.org.id, parent.num).await.unwrap().unwrap();
    assert_eq!(p.progress, Some((0, 4)));
    assert_eq!(kids[0].progress, None);
    let c = set_state(&s, &o, kids[0].num, St::Done).await;
    assert!(c.children_done.is_none());
    set_state(&s, &o, kids[1].num, St::Cancelled).await;
    let p = s.task(o.org.id, parent.num).await.unwrap().unwrap();
    // Cancelled children are left out of the total.
    assert_eq!(p.progress, Some((1, 3)));
    // Closing the parent while children are open is refused, naming them.
    let close = TaskChange {
        state: Some(St::Done),
        ..Default::default()
    };
    let e = s
        .change_task(&w(&o, parent.num, &o.a), &close)
        .await
        .unwrap_err();
    let StoreError::Refused("children_open", msg) = e else {
        panic!("{e:?}")
    };
    assert!(
        msg.contains(&format!("t{}", kids[2].num)) && msg.contains(&format!("t{}", kids[3].num)),
        "{msg}"
    );
    // The last open child closes: the parent is named once.
    set_state(&s, &o, kids[2].num, St::Done).await;
    let c = set_state(&s, &o, kids[3].num, St::Done).await;
    let (pp, n) = c.children_done.expect("children done");
    assert_eq!((pp.num, n), (parent.num, 4));
    assert_eq!(pp.progress, Some((3, 3)));
    // Reopen one and force the parent closed: the event names the child.
    set_state(&s, &o, kids[3].num, St::Open).await;
    let c = s
        .change_task(
            &w(&o, parent.num, &o.a),
            &TaskChange {
                force: true,
                ..close.clone()
            },
        )
        .await
        .unwrap();
    assert_eq!(c.task.state, St::Done);
    let e = events(&s, &o, parent.num)
        .await
        .into_iter()
        .rfind(|e| e.field.as_deref() == Some("state"))
        .unwrap();
    assert!(e.note.unwrap().contains(&format!("t{}", kids[3].num)));
    // Closing the parent changed no child.
    let k = s.task(o.org.id, kids[3].num).await.unwrap().unwrap();
    assert_eq!(k.state, St::Open);
    // The last child closing under a closed parent names nobody.
    let c = set_state(&s, &o, kids[3].num, St::Done).await;
    assert!(c.children_done.is_none());
}

async fn results(s: S) {
    use amail_proto::TaskResult as R;
    let o = setup(&s, "alpha").await;
    let t = new(&s, &o, "review it", Some(&o.a)).await;
    let res = |r: Option<R>, rf: Option<&str>, note: Option<&str>| TaskChange {
        result: Some(r),
        result_ref: rf.map(str::to_string),
        result_note: note.map(str::to_string),
        ..Default::default()
    };
    // Not on a task that is not done.
    let e = s
        .change_task(&w(&o, t.num, &o.a), &res(Some(R::Ok), None, None))
        .await
        .unwrap_err();
    assert_eq!(code(e), "not_done");
    // Together with closing.
    let c = s
        .change_task(
            &w(&o, t.num, &o.a),
            &TaskChange {
                state: Some(St::Done),
                ..res(Some(R::Rejected), Some("abc123"), Some("fails on arm"))
            },
        )
        .await
        .unwrap();
    assert_eq!(c.task.result, Some(R::Rejected));
    assert_eq!(c.task.result_ref.as_deref(), Some("abc123"));
    assert_eq!(c.task.result_by.as_deref(), Some("a"));
    assert!(c.task.result_at.is_some());
    let ev = events(&s, &o, t.num).await;
    let r = ev
        .iter()
        .find(|e| e.field.as_deref() == Some("result"))
        .unwrap();
    assert_eq!(
        (r.from.as_deref(), r.to.as_deref()),
        (None, Some("rejected"))
    );
    assert_eq!(r.note.as_deref(), Some("fails on arm"));
    let rf = ev
        .iter()
        .find(|e| e.field.as_deref() == Some("result_ref"))
        .unwrap();
    assert_eq!(rf.to.as_deref(), Some("abc123"));
    // On a task already done; by another seat.
    let c = s
        .change_task(&w(&o, t.num, &o.b), &res(Some(R::Ok), Some("def456"), None))
        .await
        .unwrap();
    assert_eq!(c.task.result, Some(R::Ok));
    assert_eq!(c.task.result_by.as_deref(), Some("b"));
    // The same again changes nothing.
    let c = s
        .change_task(&w(&o, t.num, &o.b), &res(Some(R::Ok), Some("def456"), None))
        .await
        .unwrap();
    assert!(c.fields.is_empty());
    // Withdraw: the task stays done.
    let c = s
        .change_task(
            &w(&o, t.num, &o.b),
            &TaskChange {
                note: Some("wrong commit".into()),
                ..res(None, None, None)
            },
        )
        .await
        .unwrap();
    assert_eq!(c.task.result, None);
    assert_eq!(c.task.result_ref, None);
    assert_eq!(c.task.state, St::Done);
    let e = events(&s, &o, t.num).await.pop().unwrap();
    assert_eq!(e.kind, K::ResultWithdrawn);
    assert_eq!(e.from.as_deref(), Some("ok def456"));
    assert_eq!(e.note.as_deref(), Some("wrong commit"));
    // Reopening clears a result.
    s.change_task(&w(&o, t.num, &o.b), &res(Some(R::Ok), None, None))
        .await
        .unwrap();
    let c = set_state(&s, &o, t.num, St::Open).await;
    assert_eq!(c.task.result, None);
    let e = events(&s, &o, t.num)
        .await
        .into_iter()
        .rfind(|e| e.field.as_deref() == Some("result"))
        .unwrap();
    assert_eq!(
        (e.kind, e.from.as_deref(), e.to.as_deref()),
        (K::Changed, Some("ok"), None)
    );
    // The list filter.
    let q = |r: Option<Option<R>>| TaskQuery {
        org_id: o.org.id,
        result: r,
        limit: 50,
        ..Default::default()
    };
    let other = new(&s, &o, "other", None).await;
    s.change_task(
        &w(&o, other.num, &o.a),
        &TaskChange {
            state: Some(St::Done),
            ..res(Some(R::Rejected), None, None)
        },
    )
    .await
    .unwrap();
    let nums = |p: TaskPage| p.tasks.iter().map(|t| t.num).collect::<Vec<_>>();
    assert_eq!(
        nums(s.tasks(&q(Some(Some(R::Rejected)))).await.unwrap()),
        [other.num]
    );
    assert!(nums(s.tasks(&q(Some(Some(R::Ok)))).await.unwrap()).is_empty());
    assert_eq!(nums(s.tasks(&q(Some(None))).await.unwrap()), [t.num]);
}

async fn needs_ok_readiness(s: S) {
    use amail_proto::TaskResult as R;
    let o = setup(&s, "alpha").await;
    let review = new(&s, &o, "review", Some(&o.b)).await;
    let integ = new(&s, &o, "integrate", Some(&o.a)).await;
    let c = s
        .set_task_dep(&w(&o, integ.num, &o.user), review.num, Some(true))
        .await
        .unwrap();
    assert_eq!(c.task.waits_on, [review.num]);
    assert_eq!(c.task.waits_on_ok, [review.num]);
    let d = s
        .task_detail(o.org.id, integ.num, 20)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(d.blockers_needs_ok, [review.num]);
    // Closed as rejected: still waiting, and the dependent is named.
    let close = |r: R| TaskChange {
        state: Some(St::Done),
        result: Some(Some(r)),
        ..Default::default()
    };
    let c = s
        .change_task(&w(&o, review.num, &o.b), &close(R::Rejected))
        .await
        .unwrap();
    assert!(c.unblocked.is_empty());
    assert_eq!(
        c.needs_ok_waiting.iter().map(|t| t.num).collect::<Vec<_>>(),
        [integ.num]
    );
    let t = s.task(o.org.id, integ.num).await.unwrap().unwrap();
    assert_eq!(t.shown_state(), St::Open);
    // The waiting and owed queries follow the rule.
    let (wl, _) = s.tasks_waiting(o.org.id, o.a.id, 50).await.unwrap();
    assert_eq!(wl.len(), 1);
    assert_eq!(wl[0].other.as_ref().unwrap().num, review.num);
    let (ow, _) = s.tasks_owed(o.org.id, o.b.id, 50).await.unwrap();
    assert_eq!(ow.len(), 1);
    // A new ok result unblocks it, once.
    let c = s
        .change_task(
            &w(&o, review.num, &o.b),
            &TaskChange {
                result: Some(Some(R::Ok)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        c.unblocked.iter().map(|t| t.num).collect::<Vec<_>>(),
        [integ.num]
    );
    assert!(c.needs_ok_waiting.is_empty());
    let t = s.task(o.org.id, integ.num).await.unwrap().unwrap();
    assert!(t.ready());
    let e = events(&s, &o, integ.num).await.pop().unwrap();
    assert_eq!(e.kind, K::Unblocked);
    // Withdrawing the ok result: waiting again, the dependent named.
    let c = s
        .change_task(
            &w(&o, review.num, &o.b),
            &TaskChange {
                result: Some(None),
                note: Some("found a bug".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(c.needs_ok_waiting.len(), 1);
    assert!(!s.task(o.org.id, integ.num).await.unwrap().unwrap().ready());
    // `--any-close` changes the dependency: any close satisfies it.
    let c = s
        .set_task_dep(&w(&o, integ.num, &o.user), review.num, Some(false))
        .await
        .unwrap();
    assert!(c.task.ready());
    assert_eq!(c.unblocked.len(), 1);
    // Repeating it without a flag changes nothing.
    let c = s
        .set_task_dep(&w(&o, integ.num, &o.user), review.num, None)
        .await
        .unwrap();
    assert!(c.fields.is_empty());
    // A cancelled blocker does not satisfy a dependency that needs ok.
    let other = new(&s, &o, "other", None).await;
    s.set_task_dep(&w(&o, integ.num, &o.user), other.num, Some(true))
        .await
        .unwrap();
    set_state(&s, &o, other.num, St::Cancelled).await;
    let t = s.task(o.org.id, integ.num).await.unwrap().unwrap();
    assert_eq!(t.waits_on, [other.num]);
}

fn import_file(o: &Org1) -> TaskImportRec {
    use amail_proto::TaskResult as R;
    let mut records = Vec::new();
    for i in 0..10 {
        records.push(ImportTaskRec {
            key: format!("ISSUE-{i}"),
            title: format!("issue {i}"),
            body: Some(format!("body {i}")),
            state: Some(if i < 3 { St::Done } else { St::Open }),
            assignee_id: Some(if i % 2 == 0 { Some(o.a.id) } else { None }),
            parent: (i >= 5).then(|| "ISSUE-4".to_string()),
            deps: if i == 9 {
                vec![
                    ("ISSUE-2".to_string(), true),
                    ("ISSUE-8".to_string(), false),
                ]
            } else {
                Vec::new()
            },
            result: (i == 2).then(|| ImportResultRec {
                result: R::Ok,
                result_ref: Some("abc123".into()),
                note: None,
                by: Some("old-reviewer".into()),
                at: Some(1_700_000_000_000),
            }),
            created_at: Some(1_600_000_000_000 + i),
            history: vec![ImportHistoryRec {
                at: 1_650_000_000_000,
                by: Some("someone".into()),
                text: format!("note {i}"),
            }],
            ..Default::default()
        });
    }
    TaskImportRec {
        org_id: o.org.id,
        actor_id: o.user.id,
        records,
        ..Default::default()
    }
}

async fn import_twice(s: S) {
    use amail_proto::TaskResult as R;
    let o = setup(&s, "alpha").await;
    let imp = import_file(&o);
    // A dry run keeps nothing.
    let dry = s
        .import_tasks(&TaskImportRec {
            dry_run: true,
            ..imp.clone()
        })
        .await
        .unwrap();
    assert!(
        dry.results
            .iter()
            .all(|r| r.2 == "created" && r.1.is_none())
    );
    assert!(s.task(o.org.id, 1).await.unwrap().is_none());
    let r = s.import_tasks(&imp).await.unwrap();
    assert_eq!(r.results.len(), 10);
    assert!(
        r.results.iter().all(|r| r.2 == "created"),
        "{:?}",
        r.results
    );
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    let page = s
        .tasks(&TaskQuery {
            org_id: o.org.id,
            limit: 100,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(page.tasks.len(), 10);
    let t2 = s
        .tasks(&TaskQuery {
            org_id: o.org.id,
            import_key: Some("ISSUE-2".into()),
            limit: 10,
            ..Default::default()
        })
        .await
        .unwrap()
        .tasks
        .pop()
        .unwrap();
    assert_eq!(t2.state, St::Done);
    assert_eq!(t2.result, Some(R::Ok));
    assert_eq!(t2.result_by.as_deref(), Some("old-reviewer"));
    assert_eq!(t2.result_at, Some(1_700_000_000_000));
    assert_eq!(t2.created_at, 1_600_000_000_002);
    let t4 = r.results[4].1.unwrap();
    let t9 = s
        .task(o.org.id, r.results[9].1.unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(t9.parent, Some(t4));
    // ISSUE-2 is done with ok; ISSUE-8 is open.
    assert_eq!(t9.waits_on, [r.results[8].1.unwrap()]);
    let p = s.task(o.org.id, t4).await.unwrap().unwrap();
    assert_eq!(p.progress, Some((0, 5)));
    let ev = events(&s, &o, t2.num).await;
    let h = ev.iter().find(|e| e.kind == K::Imported).unwrap();
    assert_eq!(h.actor.as_deref(), Some("someone"));
    assert_eq!(h.note.as_deref(), Some("note 2"));
    assert_eq!(h.at, 1_650_000_000_000);
    // The same file again: the same tasks, nothing changed.
    let again = s.import_tasks(&imp).await.unwrap();
    assert!(
        again.results.iter().all(|x| x.2 == "same"),
        "{:?}",
        again.results
    );
    assert_eq!(
        again.results.iter().map(|x| x.1).collect::<Vec<_>>(),
        r.results.iter().map(|x| x.1).collect::<Vec<_>>()
    );
    assert_eq!(events(&s, &o, t2.num).await.len(), ev.len());
    let page = s
        .tasks(&TaskQuery {
            org_id: o.org.id,
            limit: 100,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(page.tasks.len(), 10);
    // A changed record updates its task.
    let mut changed = imp.clone();
    changed.records[3].title = "issue 3, renamed".into();
    changed.records[3].state = Some(St::Doing);
    let r3 = s.import_tasks(&changed).await.unwrap();
    assert_eq!(r3.results[3].2, "updated");
    assert_eq!(r3.results.iter().filter(|x| x.2 == "same").count(), 9);
    let t3 = s
        .task(o.org.id, r3.results[3].1.unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (t3.title.as_str(), t3.state),
        ("issue 3, renamed", St::Doing)
    );
    // Unknown parents and blockers are warnings.
    let mut bad = imp.clone();
    bad.records[0].parent = Some("NOPE".into());
    bad.records[1].deps = vec![("GONE".into(), false)];
    let rb = s.import_tasks(&bad).await.unwrap();
    assert_eq!(rb.warnings.len(), 2, "{:?}", rb.warnings);
    assert!(rb.warnings[0].starts_with("ISSUE-0: no task imported as \"NOPE\""));
}

async fn report(s: S) {
    use amail_proto::TaskResult as R;
    let o = setup(&s, "alpha").await;
    let not_started = new(&s, &o, "not started", Some(&o.a)).await;
    let doing = new(&s, &o, "doing", Some(&o.b)).await;
    set_state(&s, &o, doing.num, St::Doing).await;
    let review = new(&s, &o, "review", Some(&o.b)).await;
    set_state(&s, &o, review.num, St::Review).await;
    let rejected = new(&s, &o, "rejected", Some(&o.a)).await;
    let approved = new(&s, &o, "approved", Some(&o.a)).await;
    let next = new(&s, &o, "next", Some(&o.b)).await;
    s.set_task_dep(&w(&o, next.num, &o.user), approved.num, Some(true))
        .await
        .unwrap();
    for (t, r) in [(&rejected, R::Rejected), (&approved, R::Ok)] {
        s.change_task(
            &w(&o, t.num, &o.a),
            &TaskChange {
                state: Some(St::Done),
                result: Some(Some(r)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }
    let parent = new(&s, &o, "parent", None).await;
    s.change_task(
        &w(&o, not_started.num, &o.user),
        &TaskChange {
            parent: Some(Some(parent.num)),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let blocked = new(&s, &o, "blocked", Some(&o.a)).await;
    s.change_task(
        &w(&o, blocked.num, &o.a),
        &TaskChange {
            state: Some(St::Blocked),
            waiting_on_id: Some(Some(o.b.id)),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let waits = new(&s, &o, "waits", Some(&o.a)).await;
    s.add_task_dep(&w(&o, waits.num, &o.user), doing.num)
        .await
        .unwrap();
    let r = s.task_report(o.org.id, None, 100).await.unwrap();
    let sec = |name: &str| {
        let x = r.sections.iter().find(|x| x.0 == name).unwrap();
        (x.1, x.2.iter().map(|i| i.task.num).collect::<Vec<_>>())
    };
    // `next` is open and assigned too; `waits` as well.
    let (n, v) = sec("not_started");
    assert!(v.contains(&not_started.num) && v.contains(&next.num) && v.contains(&waits.num));
    assert_eq!(n, v.len() as i64);
    assert_eq!(sec("in_progress").1, [doing.num]);
    assert_eq!(sec("review").1, [review.num]);
    let ri = &r.sections.iter().find(|x| x.0 == "review").unwrap().2[0];
    assert_eq!(ri.seat.as_deref(), Some("owner"));
    assert_eq!(sec("rejected").1, [rejected.num]);
    assert_eq!(sec("approved").1, [approved.num]);
    let ai = &r.sections.iter().find(|x| x.0 == "approved").unwrap().2[0];
    assert_eq!(
        ai.dependents.iter().map(|t| t.num).collect::<Vec<_>>(),
        [next.num]
    );
    assert_eq!(sec("parents").1, [parent.num]);
    let (_, b) = sec("blocked");
    assert_eq!(b, [blocked.num, waits.num]);
    let bi = &r.sections.iter().find(|x| x.0 == "blocked").unwrap().2;
    assert_eq!(bi[0].waiting_for, ["b"]);
    assert_eq!(bi[1].waiting_for, ["b"]);
}
