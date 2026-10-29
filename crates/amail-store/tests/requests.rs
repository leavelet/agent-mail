//! Requests for the person (M10), store conformance: creation and the
//! format rules, answers with `if_rev` and the one-answer rule,
//! withdrawal, the deadline pass with and without defaults, listings,
//! live attention requests, the task marker `blocked-on-human`, linked
//! threads, the storage quota, the public bounds and org deletion. Every
//! test runs against SQLite and Postgres (Postgres only when
//! `AMAIL_TEST_PG_URL` is set; otherwise it is skipped with a notice).

use amail_proto::{
    RequestClass, RequestEventKind, RequestKind, RequestOption, RequestSource, RequestState,
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
    create_and_format_rules,
    answers_and_if_rev,
    withdraw,
    deadline_pass,
    listing_order_and_filters,
    attention_requests,
    tasks_blocked_on_human,
    threads,
    storage_quota,
    public_bounds,
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

fn opt(key: &str, label: &str) -> RequestOption {
    RequestOption {
        key: key.into(),
        label: label.into(),
        description: None,
    }
}

fn ask(o: &Org1, seat: &Seat, title: &str) -> NewRequestRec {
    NewRequestRec {
        org_id: o.org.id,
        kind: RequestKind::Question,
        seat_id: seat.id,
        asker_id: Some(seat.id),
        task: None,
        title: title.into(),
        body: "Which way?".into(),
        options: vec![opt("yes", "Ship it"), opt("no", "Hold it")],
        free_text: false,
        default: None,
        class: RequestClass::Soon,
        deadline: None,
        source: RequestSource::Cli,
        attention: None,
        quota: None,
        bounds: RequestBounds::default(),
    }
}

fn code(e: StoreError) -> &'static str {
    match e {
        StoreError::Refused(c, _) => c,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn answer(o: &Org1, num: i64, option: Option<&str>, text: Option<&str>) -> RequestAnswerRec {
    RequestAnswerRec {
        org_id: o.org.id,
        num,
        option: option.map(str::to_string),
        text: text.map(str::to_string),
        by: "owner".into(),
        ..Default::default()
    }
}

async fn create_and_format_rules(s: S) {
    let o = setup(&s, "alpha").await;
    let (r, existing) = s
        .create_request(&ask(&o, &o.a, "  Ship v2?  "))
        .await
        .unwrap();
    assert!(!existing);
    assert_eq!(r.num, 1);
    assert_eq!(r.title, "Ship v2?");
    assert_eq!(r.seat, "a");
    assert_eq!(r.asker.as_deref(), Some("a"));
    assert_eq!(r.state, RequestState::Open);
    assert_eq!(r.rev, 1);
    assert_eq!(r.options.len(), 2);
    // The deadline from the class; without a default it is the first
    // reminder.
    let d = r.deadline.unwrap() - r.created_at;
    assert_eq!(d, RequestClass::Soon.deadline_ms());
    assert_eq!(r.remind_at, r.deadline);
    let evs = s.request_events(o.org.id, 1, 10).await.unwrap();
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].kind, RequestEventKind::Created);
    assert_eq!(evs[0].actor.as_deref(), Some("a"));
    let org = s.org_by_id(o.org.id).await.unwrap().unwrap();
    assert!(org.request_bytes > 256, "{}", org.request_bytes);
    // Numbers go on.
    let (r2, _) = s.create_request(&ask(&o, &o.b, "second")).await.unwrap();
    assert_eq!(r2.num, 2);

    // Format rules.
    type Change = Box<dyn Fn(&mut NewRequestRec)>;
    let cases: Vec<(&str, Change)> = vec![
        ("title_empty", Box::new(|n| n.title = "  ".into())),
        ("invalid_title", Box::new(|n| n.title = "a\nb".into())),
        ("title_too_long", Box::new(|n| n.title = "t".repeat(513))),
        (
            "body_too_large",
            Box::new(|n| n.body = "b".repeat(64 * 1024 + 1)),
        ),
        ("options_count", Box::new(|n| n.options.truncate(1))),
        ("options_count", Box::new(|n| n.options.clear())),
        (
            "options_count",
            Box::new(|n| {
                n.options = (0..7).map(|i| opt(&format!("k{i}"), "x")).collect();
            }),
        ),
        (
            "invalid_option_key",
            Box::new(|n| n.options[0].key = "12".into()),
        ),
        (
            "invalid_option_key",
            Box::new(|n| n.options[0].key = "a b".into()),
        ),
        (
            "invalid_option_key",
            Box::new(|n| n.options[0].key = "k".repeat(33)),
        ),
        (
            "duplicate_option",
            Box::new(|n| n.options[1].key = "YES".into()),
        ),
        (
            "invalid_option_label",
            Box::new(|n| n.options[0].label = " ".into()),
        ),
        (
            "option_label_too_long",
            Box::new(|n| n.options[0].label = "l".repeat(201)),
        ),
        (
            "option_description_too_long",
            Box::new(|n| n.options[0].description = Some("d".repeat(1001))),
        ),
        (
            "invalid_default",
            Box::new(|n| n.default = Some("maybe".into())),
        ),
        (
            "free_text_not_allowed",
            Box::new(|n| {
                n.kind = RequestKind::Merge;
                n.free_text = true;
            }),
        ),
    ];
    for (want, f) in cases {
        let mut n = ask(&o, &o.a, "x");
        f(&mut n);
        let e = s.create_request(&n).await.unwrap_err();
        assert_eq!(code(e), want);
    }
    // At the bounds: 6 options, 32-character keys, 200-character labels.
    let mut n = ask(&o, &o.a, &"t".repeat(512));
    n.options = (0..6)
        .map(|i| opt(&format!("{i}{}", "k".repeat(31)), &"l".repeat(200)))
        .collect();
    n.body = "b".repeat(64 * 1024);
    s.create_request(&n).await.unwrap();
    // Free text without options.
    let mut n = ask(&o, &o.a, "free");
    n.options.clear();
    n.free_text = true;
    let (r, _) = s.create_request(&n).await.unwrap();
    assert!(r.free_text);
    // An unknown task.
    let mut n = ask(&o, &o.a, "x");
    n.task = Some(99);
    assert!(matches!(
        s.create_request(&n).await.unwrap_err(),
        StoreError::NotFound(_)
    ));
    // Refusals take no number.
    let (r, _) = s.create_request(&ask(&o, &o.a, "next")).await.unwrap();
    assert_eq!(r.num, 5);
}

async fn answers_and_if_rev(s: S) {
    let o = setup(&s, "alpha").await;
    let (r, _) = s.create_request(&ask(&o, &o.a, "ship?")).await.unwrap();
    // Wrong revision.
    let mut a = answer(&o, r.num, Some("yes"), None);
    a.if_rev = Some(r.rev + 1);
    assert_eq!(code(s.answer_request(&a).await.unwrap_err()), "stale");
    // Not an option; nothing; text where options are wanted.
    let e = s
        .answer_request(&answer(&o, r.num, Some("maybe"), None))
        .await
        .unwrap_err();
    assert_eq!(code(e), "invalid_option");
    let e = s
        .answer_request(&answer(&o, r.num, None, None))
        .await
        .unwrap_err();
    assert_eq!(code(e), "answer_required");
    let e = s
        .answer_request(&answer(&o, r.num, None, Some("sure")))
        .await
        .unwrap_err();
    assert_eq!(code(e), "free_text_not_allowed");
    // Answered at the right revision, with a note.
    let mut a = answer(&o, r.num, Some("no"), None);
    a.if_rev = Some(r.rev);
    a.note = Some(" wait for the review ".into());
    let done = s.answer_request(&a).await.unwrap();
    assert_eq!(done.state, RequestState::Answered);
    assert_eq!(done.answer_key.as_deref(), Some("no"));
    assert_eq!(done.answer_note.as_deref(), Some("wait for the review"));
    assert_eq!(done.answer_by.as_deref(), Some("owner"));
    assert!(done.answered_at.is_some());
    assert_eq!(done.rev, r.rev + 1);
    assert_eq!(done.remind_at, None);
    // A second answer is refused; the first stays.
    let e = s
        .answer_request(&answer(&o, r.num, Some("yes"), None))
        .await
        .unwrap_err();
    assert_eq!(code(e), "answered");
    let now = s.request(o.org.id, r.num).await.unwrap().unwrap();
    assert_eq!(now.answer_key.as_deref(), Some("no"));
    // Free text.
    let mut n = ask(&o, &o.a, "name it");
    n.options.clear();
    n.free_text = true;
    let (r, _) = s.create_request(&n).await.unwrap();
    let e = s
        .answer_request(&answer(&o, r.num, None, Some(&"x".repeat(16_385))))
        .await
        .unwrap_err();
    assert_eq!(code(e), "answer_too_long");
    let done = s
        .answer_request(&answer(&o, r.num, None, Some("call it lark")))
        .await
        .unwrap();
    assert_eq!(done.answer_text.as_deref(), Some("call it lark"));
    let evs = s.request_events(o.org.id, r.num, 10).await.unwrap();
    assert_eq!(evs.last().unwrap().kind, RequestEventKind::Answered);
    assert_eq!(evs.last().unwrap().actor.as_deref(), Some("owner"));
    // No such request.
    assert!(matches!(
        s.answer_request(&answer(&o, 99, Some("yes"), None))
            .await
            .unwrap_err(),
        StoreError::NotFound(_)
    ));
    // Concurrent answers: exactly one wins.
    let (r, _) = s.create_request(&ask(&o, &o.a, "race")).await.unwrap();
    let mut handles = Vec::new();
    for k in ["yes", "no", "yes", "no"] {
        let s = s.clone();
        let a = answer(&o, r.num, Some(k), None);
        handles.push(tokio::spawn(async move { s.answer_request(&a).await }));
    }
    let mut ok = 0;
    for h in handles {
        match h.await.unwrap() {
            Ok(_) => ok += 1,
            Err(e) => assert_eq!(code(e), "answered"),
        }
    }
    assert_eq!(ok, 1);
}

async fn withdraw(s: S) {
    let o = setup(&s, "alpha").await;
    let (r, _) = s.create_request(&ask(&o, &o.a, "ship?")).await.unwrap();
    let w = s
        .withdraw_request(o.org.id, r.num, "a", Some("found it myself"))
        .await
        .unwrap();
    assert_eq!(w.state, RequestState::Withdrawn);
    assert_eq!(w.remind_at, None);
    let e = s
        .withdraw_request(o.org.id, r.num, "a", None)
        .await
        .unwrap_err();
    assert_eq!(code(e), "not_open");
    let e = s
        .answer_request(&answer(&o, r.num, Some("yes"), None))
        .await
        .unwrap_err();
    assert_eq!(code(e), "not_open");
    let evs = s.request_events(o.org.id, r.num, 10).await.unwrap();
    assert_eq!(evs[1].kind, RequestEventKind::Withdrawn);
    assert_eq!(evs[1].note.as_deref(), Some("found it myself"));
}

async fn deadline_pass(s: S) {
    let o = setup(&s, "alpha").await;
    let now = now_ms();
    // With a default and a passed deadline: the default is taken.
    let mut n = ask(&o, &o.a, "with default");
    n.default = Some("no".into());
    n.deadline = Some(now - 1000);
    let (d, _) = s.create_request(&n).await.unwrap();
    assert_eq!(d.remind_at, None);
    // Without a default: reminded.
    let mut n = ask(&o, &o.b, "without default");
    n.class = RequestClass::Now;
    n.deadline = Some(now - 1000);
    let (nd, _) = s.create_request(&n).await.unwrap();
    // Not due yet.
    let mut n = ask(&o, &o.a, "later");
    n.default = Some("yes".into());
    let (later, _) = s.create_request(&n).await.unwrap();

    let due = s.claim_due_requests(now, 100).await.unwrap();
    assert_eq!(
        due.expired.iter().map(|r| r.num).collect::<Vec<_>>(),
        vec![d.num]
    );
    let e = &due.expired[0];
    assert_eq!(e.state, RequestState::Expired);
    assert_eq!(e.answer_key.as_deref(), Some("no"));
    assert_eq!(e.answer_by.as_deref(), Some("deadline"));
    assert_eq!(
        due.remind.iter().map(|r| r.num).collect::<Vec<_>>(),
        vec![nd.num]
    );
    let r = &due.remind[0];
    assert_eq!(r.state, RequestState::Open);
    // `now`: the next reminder in an hour.
    assert_eq!(r.remind_at, Some(now + 3_600_000));
    // Each once.
    let again = s.claim_due_requests(now, 100).await.unwrap();
    assert!(again.expired.is_empty() && again.remind.is_empty());
    // An hour later, reminded again; the other still waits.
    let again = s.claim_due_requests(now + 3_600_000, 100).await.unwrap();
    assert_eq!(again.remind.len(), 1);
    assert!(again.expired.is_empty());
    assert_eq!(
        s.request(o.org.id, later.num).await.unwrap().unwrap().state,
        RequestState::Open
    );
    // An expired request counts as answered.
    let e = s
        .answer_request(&answer(&o, d.num, Some("yes"), None))
        .await
        .unwrap_err();
    assert_eq!(code(e), "answered");
    let evs = s.request_events(o.org.id, nd.num, 10).await.unwrap();
    assert_eq!(
        evs.iter().map(|e| e.kind).collect::<Vec<_>>(),
        vec![
            RequestEventKind::Created,
            RequestEventKind::Reminded,
            RequestEventKind::Reminded
        ]
    );
    // Attention requests have no deadline.
    let mut n = ask(&o, &o.a, "a needs a person: login");
    n.kind = RequestKind::Attention;
    n.asker_id = None;
    n.attention = Some("login".into());
    n.source = RequestSource::Node;
    let (at, _) = s.create_request(&n).await.unwrap();
    assert_eq!((at.deadline, at.remind_at), (None, None));
}

async fn listing_order_and_filters(s: S) {
    let o = setup(&s, "alpha").await;
    let mk = |seat: &Seat, title: &str, class: RequestClass| {
        let mut n = ask(&o, seat, title);
        n.class = class;
        n
    };
    let later = s
        .create_request(&mk(&o.a, "later", RequestClass::Later))
        .await
        .unwrap()
        .0;
    let soon = s
        .create_request(&mk(&o.b, "soon", RequestClass::Soon))
        .await
        .unwrap()
        .0;
    let now1 = s
        .create_request(&mk(&o.a, "now1", RequestClass::Now))
        .await
        .unwrap()
        .0;
    let now2 = s
        .create_request(&mk(&o.b, "now2", RequestClass::Now))
        .await
        .unwrap()
        .0;
    let done = s
        .create_request(&mk(&o.a, "done", RequestClass::Now))
        .await
        .unwrap()
        .0;
    s.answer_request(&answer(&o, done.num, Some("yes"), None))
        .await
        .unwrap();
    let q = RequestQuery {
        org_id: o.org.id,
        limit: 100,
        ..Default::default()
    };
    let nums = |v: Vec<RequestRec>| v.into_iter().map(|r| r.num).collect::<Vec<_>>();
    let (all, truncated) = s.requests(&q).await.unwrap();
    assert!(!truncated);
    assert_eq!(
        nums(all),
        vec![now1.num, now2.num, soon.num, later.num, done.num]
    );
    let (open, _) = s
        .requests(&RequestQuery {
            states: vec![RequestState::Open],
            ..q.clone()
        })
        .await
        .unwrap();
    assert_eq!(open.len(), 4);
    let (a, _) = s
        .requests(&RequestQuery {
            seat_id: Some(o.a.id),
            states: vec![RequestState::Open],
            ..q.clone()
        })
        .await
        .unwrap();
    assert_eq!(nums(a), vec![now1.num, later.num]);
    let (c, _) = s
        .requests(&RequestQuery {
            class: Some(RequestClass::Now),
            ..q.clone()
        })
        .await
        .unwrap();
    assert_eq!(nums(c), vec![now1.num, now2.num, done.num]);
    let (k, _) = s
        .requests(&RequestQuery {
            kind: Some(RequestKind::Plan),
            ..q.clone()
        })
        .await
        .unwrap();
    assert!(k.is_empty());
    let (cut, truncated) = s
        .requests(&RequestQuery {
            limit: 2,
            ..q.clone()
        })
        .await
        .unwrap();
    assert!(truncated);
    assert_eq!(cut.len(), 2);
    // `mine`: about the seat or made by it.
    let mut n = ask(&o, &o.b, "made by a for b");
    n.asker_id = Some(o.a.id);
    let by_a = s.create_request(&n).await.unwrap().0;
    let (m, _) = s
        .requests(&RequestQuery {
            mine: Some(o.a.id),
            states: vec![RequestState::Open],
            ..q.clone()
        })
        .await
        .unwrap();
    assert_eq!(nums(m), vec![now1.num, by_a.num, later.num]);
}

async fn attention_requests(s: S) {
    let o = setup(&s, "alpha").await;
    let mk = |seat: &Seat, code: &str| {
        let mut n = ask(&o, seat, &format!("{} needs a person: {code}", seat.name));
        n.kind = RequestKind::Attention;
        n.asker_id = None;
        n.attention = Some(code.into());
        n.source = RequestSource::Node;
        n.class = RequestClass::Now;
        n.options = amail_proto::ATTENTION_OPTIONS
            .iter()
            .map(|(k, l)| opt(k, l))
            .collect();
        n
    };
    let (r1, e1) = s.create_request(&mk(&o.a, "login")).await.unwrap();
    assert!(!e1 && r1.attention_live);
    assert_eq!(r1.asker, None);
    // The same seat and code: the live one.
    let (r2, e2) = s.create_request(&mk(&o.a, "login")).await.unwrap();
    assert!(e2);
    assert_eq!(r2.num, r1.num);
    // Another code, another seat: new ones.
    let (r3, _) = s.create_request(&mk(&o.a, "trust-prompt")).await.unwrap();
    let (r4, _) = s.create_request(&mk(&o.b, "login")).await.unwrap();
    assert_ne!(r3.num, r1.num);
    assert_ne!(r4.num, r1.num);
    let live = s.live_attention_requests(o.org.id).await.unwrap();
    assert_eq!(live.len(), 3);
    // The condition is gone: withdrawn by the node, no longer live.
    let ended = s
        .end_attention_request(o.org.id, r1.num)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ended.state, RequestState::Withdrawn);
    assert!(!ended.attention_live);
    assert!(
        s.end_attention_request(o.org.id, r1.num)
            .await
            .unwrap()
            .is_none()
    );
    // An answered request stays live while the condition lasts (no new
    // request), and ends without changing its state.
    s.answer_request(&answer(&o, r3.num, Some("dismiss"), None))
        .await
        .unwrap();
    let (again, existing) = s.create_request(&mk(&o.a, "trust-prompt")).await.unwrap();
    assert!(existing);
    assert_eq!(again.state, RequestState::Answered);
    let ended = s
        .end_attention_request(o.org.id, r3.num)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ended.state, RequestState::Answered);
    // The condition comes back: a new request.
    let (r5, existing) = s.create_request(&mk(&o.a, "login")).await.unwrap();
    assert!(!existing);
    assert!(r5.num > r4.num);
    assert_eq!(
        s.live_attention_requests(o.org.id)
            .await
            .unwrap()
            .iter()
            .map(|r| r.num)
            .collect::<Vec<_>>(),
        vec![r4.num, r5.num]
    );
}

async fn tasks_blocked_on_human(s: S) {
    let o = setup(&s, "alpha").await;
    let t = s
        .create_task(&NewTaskRec {
            org_id: o.org.id,
            reporter_id: o.user.id,
            title: "pool task".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let t2 = s
        .create_task(&NewTaskRec {
            org_id: o.org.id,
            reporter_id: o.user.id,
            title: "assigned task".into(),
            assignee_id: Some(o.a.id),
            ..Default::default()
        })
        .await
        .unwrap();
    let mut n = ask(&o, &o.a, "approve the plan");
    n.kind = RequestKind::Plan;
    n.task = Some(t.num);
    let (r, _) = s.create_request(&n).await.unwrap();
    assert_eq!(r.task, Some(t.num));
    let mut n = ask(&o, &o.a, "second");
    n.task = Some(t.num);
    s.create_request(&n).await.unwrap();
    let mut n = ask(&o, &o.a, "on t2");
    n.task = Some(t2.num);
    let (r2, _) = s.create_request(&n).await.unwrap();
    // The marker: the oldest open request.
    let got = s.task(o.org.id, t.num).await.unwrap().unwrap();
    assert_eq!(got.blocked_on_human, Some(r.num));
    let page = s
        .tasks(&TaskQuery {
            org_id: o.org.id,
            limit: 50,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(page.tasks.iter().all(|x| x.blocked_on_human.is_some()));
    let page = s
        .tasks(&TaskQuery {
            org_id: o.org.id,
            limit: 50,
            skip_blocked_on_human: true,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(page.tasks.is_empty());
    // Take and steal skip it.
    let w = |num: i64, seat: &Seat| TaskWrite {
        org_id: o.org.id,
        num,
        actor_id: Some(seat.id),
        ..Default::default()
    };
    assert_eq!(
        code(s.take_task(&w(t.num, &o.b)).await.unwrap_err()),
        "blocked_on_human"
    );
    assert_eq!(
        code(s.steal_task(&w(t2.num, &o.b)).await.unwrap_err()),
        "blocked_on_human"
    );
    // Answered: the next open one; then none.
    s.answer_request(&answer(&o, r.num, Some("yes"), None))
        .await
        .unwrap();
    let got = s.task(o.org.id, t.num).await.unwrap().unwrap();
    assert_eq!(got.blocked_on_human, Some(r.num + 1));
    s.withdraw_request(o.org.id, r.num + 1, "a", None)
        .await
        .unwrap();
    s.withdraw_request(o.org.id, r2.num, "a", None)
        .await
        .unwrap();
    let got = s.task(o.org.id, t.num).await.unwrap().unwrap();
    assert_eq!(got.blocked_on_human, None);
    s.take_task(&w(t.num, &o.b)).await.unwrap();
    s.steal_task(&w(t2.num, &o.b)).await.unwrap();
    // Listed by task number.
    let (by_task, _) = s
        .requests(&RequestQuery {
            org_id: o.org.id,
            task: Some(t.num),
            limit: 10,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(by_task.len(), 2);
}

async fn threads(s: S) {
    let o = setup(&s, "alpha").await;
    let (r, _) = s.create_request(&ask(&o, &o.a, "ship?")).await.unwrap();
    s.set_request_thread(o.org.id, r.num, 500, 500)
        .await
        .unwrap();
    let r = s.request(o.org.id, r.num).await.unwrap().unwrap();
    assert_eq!((r.thread_id, r.message_id), (Some(500), Some(500)));
    let by = s.thread_requests(o.org.id, 500).await.unwrap();
    assert_eq!(by.len(), 1);
    assert!(s.thread_requests(o.org.id, 501).await.unwrap().is_empty());
    // Links: the own thread is not linked; another once.
    assert!(
        !s.link_request_thread(o.org.id, r.num, 500, Some("b"), "own")
            .await
            .unwrap()
    );
    assert!(
        s.link_request_thread(o.org.id, r.num, 700, Some("b"), "[r1] in a subject")
            .await
            .unwrap()
    );
    assert!(
        !s.link_request_thread(o.org.id, r.num, 700, Some("b"), "again")
            .await
            .unwrap()
    );
    assert!(
        !s.link_request_thread(o.org.id, 99, 700, Some("b"), "none")
            .await
            .unwrap()
    );
    assert_eq!(s.request_threads(o.org.id, r.num).await.unwrap(), vec![700]);
    // A comment event.
    let c = s
        .record_request_event(
            o.org.id,
            r.num,
            Some("owner"),
            RequestEventKind::Commented,
            None,
            Some(42),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(c.state, RequestState::Open);
    let evs = s.request_events(o.org.id, r.num, 10).await.unwrap();
    assert_eq!(
        evs.iter().map(|e| e.kind).collect::<Vec<_>>(),
        vec![
            RequestEventKind::Created,
            RequestEventKind::Linked,
            RequestEventKind::Commented
        ]
    );
    assert_eq!(evs[2].message_id, Some(42));
    // The newest `limit` events, oldest first.
    let last = s.request_events(o.org.id, r.num, 1).await.unwrap();
    assert_eq!(last[0].kind, RequestEventKind::Commented);
}

async fn storage_quota(s: S) {
    let o = setup(&s, "alpha").await;
    s.create_request(&ask(&o, &o.a, "one")).await.unwrap();
    let org = s.org_by_id(o.org.id).await.unwrap().unwrap();
    let used = org.stored_bytes + org.task_bytes + org.doc_bytes + org.request_bytes;
    let mut n = ask(&o, &o.a, "two");
    n.quota = Some(used + 10);
    assert!(matches!(
        s.create_request(&n).await.unwrap_err(),
        StoreError::StorageQuota(_)
    ));
    n.quota = Some(used + 10_000);
    s.create_request(&n).await.unwrap();
    // Request bytes count against the quota of mail too.
    let org = s.org_by_id(o.org.id).await.unwrap().unwrap();
    assert!(org.request_bytes > 0);
}

async fn public_bounds(s: S) {
    let o = setup(&s, "alpha").await;
    let mut n = ask(&o, &o.a, "x");
    n.bounds = RequestBounds {
        open_per_org: Some(2),
        per_seat_per_hour: None,
    };
    s.create_request(&n).await.unwrap();
    s.create_request(&n).await.unwrap();
    assert_eq!(
        code(s.create_request(&n).await.unwrap_err()),
        "too_many_open_requests"
    );
    // An answered one makes room.
    s.answer_request(&answer(&o, 1, Some("yes"), None))
        .await
        .unwrap();
    s.create_request(&n).await.unwrap();
    // Per seat and hour: counted per asker.
    let mut n = ask(&o, &o.b, "y");
    n.bounds = RequestBounds {
        open_per_org: None,
        per_seat_per_hour: Some(1),
    };
    s.create_request(&n).await.unwrap();
    assert_eq!(
        code(s.create_request(&n).await.unwrap_err()),
        "request_rate"
    );
    // The user (no asker counted) is not limited.
    n.asker_id = None;
    s.create_request(&n).await.unwrap();
}

async fn org_deletion(s: S) {
    let o = setup(&s, "alpha").await;
    let t = s
        .create_task(&NewTaskRec {
            org_id: o.org.id,
            reporter_id: o.user.id,
            title: "t".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let mut n = ask(&o, &o.a, "ship?");
    n.task = Some(t.num);
    let (r, _) = s.create_request(&n).await.unwrap();
    s.link_request_thread(o.org.id, r.num, 9, None, "x")
        .await
        .unwrap();
    assert!(s.delete_org(o.org.id).await.unwrap());
    let o2 = setup(&s, "alpha").await;
    assert!(s.request(o2.org.id, 1).await.unwrap().is_none());
    let (r, _) = s.create_request(&ask(&o2, &o2.a, "again")).await.unwrap();
    assert_eq!(r.num, 1);
}
