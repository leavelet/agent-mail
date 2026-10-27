//! Document store conformance (M9). Every test runs against SQLite and
//! Postgres (Postgres only when `AMAIL_TEST_PG_URL` is set; otherwise it is
//! skipped with a notice).

use amail_proto::{DocEventKind as K, DocStatus as St};
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
    kinds_numbers_and_slugs,
    required_sections_and_sizes,
    revisions_if_rev_and_noop,
    sections_written_alone,
    idempotent_create,
    write_rights_and_frozen_decisions,
    relations_and_supersession,
    links_dangling_and_stale,
    split_rewrites_links,
    extract_and_merge,
    search_by_section,
    index_order_and_scope,
    candidates_on_create,
    expiry_and_dangling_claims,
    comments_on_removed_sections,
    quota_and_deletion,
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

/// A write as seat `actor` (the user seat writes as the user).
fn w(o: &Org1, num: i64, actor: &Seat) -> DocWrite {
    DocWrite {
        org_id: o.org.id,
        num,
        actor_id: Some(actor.id),
        is_user: actor.id == o.user.id,
        ..Default::default()
    }
}

fn nd(o: &Org1, owner: &Seat, kind: &str, title: &str, body: &str) -> NewDocRec {
    NewDocRec {
        w: w(o, 0, owner),
        owner_id: owner.id,
        kind: kind.into(),
        title: title.into(),
        body: body.into(),
        ..Default::default()
    }
}

async fn new(s: &S, o: &Org1, owner: &Seat, kind: &str, title: &str, body: &str) -> DocRec {
    s.create_doc(&nd(o, owner, kind, title, body))
        .await
        .unwrap()
        .doc
}

async fn body(s: &S, o: &Org1, num: i64) -> String {
    s.doc(o.org.id, num).await.unwrap().unwrap().body
}

async fn set(s: &S, o: &Org1, num: i64, actor: &Seat, ch: DocChange) -> Result<DocChanged> {
    s.change_doc(&w(o, num, actor), &ch).await
}

async fn write(s: &S, o: &Org1, num: i64, actor: &Seat, text: &str) -> Result<DocChanged> {
    s.write_doc_body(
        &w(o, num, actor),
        &DocBodyRec {
            body: text.into(),
            ..Default::default()
        },
    )
    .await
}

fn code(e: StoreError) -> &'static str {
    match e {
        StoreError::Refused(c, _) => c,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

const FINDING: &str = "# Observed\n\nx\n\n# Evidence\n\ny\n\n# Applies to\n\nz\n";
const DECISION: &str =
    "# Context\n\nc\n\n# Decision\n\nd\n\n# Alternatives\n\na\n\n# Consequences\n\nq\n";

async fn kinds_numbers_and_slugs(s: S) {
    let o = setup(&s, "alpha").await;
    let kinds = s.doc_kinds(o.org.id).await.unwrap();
    let names: Vec<&str> = kinds.iter().map(|k| k.name.as_str()).collect();
    assert_eq!(
        names,
        ["decision", "finding", "runbook", "index", "reference"]
    );
    let decision = &kinds[0];
    assert_eq!(decision.activate_by, ActivateBy::Human);
    assert_eq!(decision.required_sections.len(), 4);
    assert!(!kinds[4].in_index);
    assert_eq!(kinds[1].verify_days, Some(30));
    let d1 = new(&s, &o, &o.a, "reference", "First", "hello").await;
    let mut n2 = nd(&o, &o.a, "reference", "Second", "");
    n2.slug = Some("runbook.deploy".into());
    let d2 = s.create_doc(&n2).await.unwrap().doc;
    assert_eq!((d1.num, d2.num), (1, 2));
    assert_eq!(d1.status, St::Draft);
    assert_eq!(d1.rev, 1);
    assert_eq!(d1.owner, "a");
    assert_eq!(d1.body, "hello");
    assert_eq!(d1.tokens, 2);
    assert_eq!(
        s.doc_num_by_slug(o.org.id, "runbook.deploy").await.unwrap(),
        Some(2)
    );
    // Slugs are unique per org; renaming keeps the number and records an event.
    let e = s.create_doc(&n2).await.unwrap_err();
    assert_eq!(code(e), "slug_taken");
    let c = set(
        &s,
        &o,
        2,
        &o.a,
        DocChange {
            slug: Some(Some("runbook.deploy.v2".into())),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(c.doc.num, 2);
    assert_eq!(c.fields, ["slug"]);
    let evs = s.doc_events(o.org.id, 2, 0, 100).await.unwrap().unwrap();
    assert!(
        evs.iter()
            .any(|e| e.kind == K::Changed && e.field.as_deref() == Some("slug"))
    );
    // Unknown and custom kinds; another org's numbers start at 1.
    let e = s
        .create_doc(&nd(&o, &o.a, "memo", "x", ""))
        .await
        .unwrap_err();
    assert_eq!(code(e), "unknown_kind");
    s.put_doc_kind(
        o.org.id,
        &DocKindPut {
            name: "memo".into(),
            required_sections: Some(vec!["Summary".into()]),
            ..Default::default()
        },
        Some(6),
    )
    .await
    .unwrap();
    let e = s
        .put_doc_kind(
            o.org.id,
            &DocKindPut {
                name: "memo2".into(),
                ..Default::default()
            },
            Some(6),
        )
        .await
        .unwrap_err();
    assert_eq!(code(e), "too_many_kinds");
    let m = new(&s, &o, &o.a, "memo", "M", "# Summary\n\nok\n").await;
    assert_eq!(m.num, 3);
    let o2 = setup(&s, "beta").await;
    assert_eq!(new(&s, &o2, &o2.a, "reference", "x", "").await.num, 1);
    assert_eq!(s.doc_kinds(o2.org.id).await.unwrap().len(), 5);
}

async fn required_sections_and_sizes(s: S) {
    let o = setup(&s, "alpha").await;
    let e = s
        .create_doc(&nd(&o, &o.a, "finding", "F", "# Observed\n\nonly\n"))
        .await
        .unwrap_err();
    match e {
        StoreError::Refused("sections_missing", m) => {
            assert!(m.contains("Evidence, Applies to"), "{m}")
        }
        other => panic!("{other:?}"),
    }
    // Case and level do not matter.
    let f = new(
        &s,
        &o,
        &o.a,
        "finding",
        "F",
        "## observed\n\nx\n\n### EVIDENCE\n\ny\n\n# Applies to\n",
    )
    .await;
    // A write that drops a section is refused too.
    let e = write(&s, &o, f.num, &o.a, "# Observed\n\nx\n")
        .await
        .unwrap_err();
    assert_eq!(code(e), "sections_missing");
    // The hard cap.
    let big = "a".repeat(amail_proto::limits::MAX_DOC_BODY_BYTES + 1);
    let e = s
        .create_doc(&nd(&o, &o.a, "reference", "big", &big))
        .await
        .unwrap_err();
    match e {
        StoreError::Refused("body_too_large", m) => {
            assert!(m.contains("524,289") && m.contains("524,288"), "{m}")
        }
        other => panic!("{other:?}"),
    }
    // At the cap it is accepted; the token estimate is one per 4 characters.
    let at = "a".repeat(amail_proto::limits::MAX_DOC_BODY_BYTES);
    let d = new(&s, &o, &o.a, "reference", "at cap", &at).await;
    assert_eq!(
        d.tokens,
        (amail_proto::limits::MAX_DOC_BODY_BYTES as i64) / 4
    );
    assert_eq!(d.body_bytes, amail_proto::limits::MAX_DOC_BODY_BYTES as i64);
    // A kind's own cap; CJK counts one token per character.
    s.put_doc_kind(
        o.org.id,
        &DocKindPut {
            name: "note".into(),
            body_cap_bytes: Some(100),
            ..Default::default()
        },
        None,
    )
    .await
    .unwrap();
    let e = s
        .create_doc(&nd(&o, &o.a, "note", "n", &"b".repeat(101)))
        .await
        .unwrap_err();
    assert_eq!(code(e), "body_too_large");
    let c = new(&s, &o, &o.a, "note", "n", "部署说明 ab").await;
    assert_eq!(c.tokens, 5);
}

async fn revisions_if_rev_and_noop(s: S) {
    let o = setup(&s, "alpha").await;
    let d = new(&s, &o, &o.a, "reference", "R", "one\n").await;
    let c = s
        .write_doc_body(
            &DocWrite {
                if_rev: Some(1),
                ..w(&o, d.num, &o.a)
            },
            &DocBodyRec {
                body: "one\ntwo\n".into(),
                message: Some("add two".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(c.revised && !c.unchanged);
    assert_eq!(c.doc.rev, 2);
    assert_eq!(c.fields, ["body"]);
    // The same body again: nothing is written.
    let c = write(&s, &o, d.num, &o.a, "one\ntwo\n").await.unwrap();
    assert!(c.unchanged && !c.revised);
    assert_eq!(c.doc.rev, 2);
    // A writer at rev 1 is refused; the events since rev 1 name the
    // revision, its author and message.
    let e = s
        .write_doc_body(
            &DocWrite {
                if_rev: Some(1),
                ..w(&o, d.num, &o.a)
            },
            &DocBodyRec {
                body: "three\n".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(code(e), "stale");
    let since = s
        .doc_events_since_rev(o.org.id, d.num, 1, 100)
        .await
        .unwrap();
    assert_eq!(since.len(), 1);
    assert_eq!(since[0].kind, K::Revised);
    assert_eq!(since[0].rev, Some(2));
    assert_eq!(since[0].actor.as_deref(), Some("a"));
    assert_eq!(since[0].note.as_deref(), Some("add two"));
    // Old revisions stay readable.
    let r1 = s.doc_revision(o.org.id, d.num, 1).await.unwrap().unwrap();
    assert_eq!(r1.body, "one\n");
    let revs = s.doc_revisions(o.org.id, d.num).await.unwrap();
    assert_eq!(revs.len(), 2);
    assert_eq!(revs[1].message.as_deref(), Some("add two"));
    assert_eq!(revs[1].sha256, amail_store::docmd::sha256_hex("one\ntwo\n"));
    // Title changes are revisions; other fields are events only.
    let c = set(
        &s,
        &o,
        d.num,
        &o.a,
        DocChange {
            title: Some("R2".into()),
            labels: Some(vec!["x".into()]),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(c.doc.rev, 3);
    assert_eq!(c.doc.labels, ["x"]);
    let c = set(
        &s,
        &o,
        d.num,
        &o.a,
        DocChange {
            pinned: Some(true),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(c.doc.rev, 3);
    assert!(c.doc.pinned);
    let diff = amail_store::docmd::unified_diff(
        &r1.body,
        &body(&s, &o, d.num).await,
        "d1 rev 1",
        "d1 rev 3",
    );
    assert!(diff.contains("+two"), "{diff}");
}

async fn sections_written_alone(s: S) {
    let o = setup(&s, "alpha").await;
    let d = new(
        &s,
        &o,
        &o.a,
        "reference",
        "R",
        "intro\n\n# Deploy\n\nold steps\n\n## Rollback\n\nundo\n\n# Other\n\nkeep\n",
    )
    .await;
    let det = s.doc_detail(o.org.id, d.num, 20).await.unwrap().unwrap();
    let anchors: Vec<&str> = det.sections.iter().map(|x| x.anchor.as_str()).collect();
    assert_eq!(anchors, ["", "deploy", "rollback", "other"]);
    assert_eq!(det.sections[2].path, "Deploy > Rollback");
    s.write_doc_body(
        &w(&o, d.num, &o.a),
        &DocBodyRec {
            body: "new undo".into(),
            section: Some("rollback".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let b = body(&s, &o, d.num).await;
    assert!(b.contains("## Rollback\n\nnew undo\n\n# Other"), "{b}");
    assert!(b.contains("old steps"));
    let e = s
        .write_doc_body(
            &w(&o, d.num, &o.a),
            &DocBodyRec {
                body: "x".into(),
                section: Some("nope".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(code(e), "no_section");
}

async fn idempotent_create(s: S) {
    let o = setup(&s, "alpha").await;
    let mut n = nd(&o, &o.a, "reference", "Same", "body");
    n.client_key = Some("k1".into());
    let first = s.create_doc(&n).await.unwrap();
    assert!(!first.existing);
    let again = s.create_doc(&n).await.unwrap();
    assert!(again.existing);
    assert_eq!(again.doc.num, first.doc.num);
    n.client_key = None;
    assert_eq!(s.create_doc(&n).await.unwrap().doc.num, 2);
}

async fn write_rights_and_frozen_decisions(s: S) {
    let o = setup(&s, "alpha").await;
    let d = new(&s, &o, &o.a, "reference", "R", "x").await;
    // Another seat cannot revise; an editor can; status is the owner's.
    let e = write(&s, &o, d.num, &o.b, "y").await.unwrap_err();
    assert_eq!(code(e), "not_owner");
    let e = set(
        &s,
        &o,
        d.num,
        &o.b,
        DocChange {
            editors: Some(vec!["b".into()]),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(code(e), "not_owner");
    set(
        &s,
        &o,
        d.num,
        &o.a,
        DocChange {
            editors: Some(vec!["b".into()]),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    write(&s, &o, d.num, &o.b, "y").await.unwrap();
    let e = set(
        &s,
        &o,
        d.num,
        &o.b,
        DocChange {
            status: Some(St::Active),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(code(e), "not_owner");
    write(&s, &o, d.num, &o.user, "z").await.unwrap();
    // The owner activates a reference; the transitions are checked.
    let c = set(
        &s,
        &o,
        d.num,
        &o.a,
        DocChange {
            status: Some(St::Active),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(c.doc.status, St::Active);
    let e = set(
        &s,
        &o,
        d.num,
        &o.a,
        DocChange {
            status: Some(St::Draft),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(code(e), "invalid_transition");
    let e = set(
        &s,
        &o,
        d.num,
        &o.a,
        DocChange {
            status: Some(St::Superseded),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(code(e), "invalid_transition");
    // A decision waits for the user; once active its body is frozen.
    let dec = new(&s, &o, &o.a, "decision", "Use X", DECISION).await;
    let e = set(
        &s,
        &o,
        dec.num,
        &o.a,
        DocChange {
            status: Some(St::Active),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(code(e), "not_owner");
    set(
        &s,
        &o,
        dec.num,
        &o.user,
        DocChange {
            status: Some(St::Active),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let e = write(&s, &o, dec.num, &o.a, &format!("{DECISION}\nmore"))
        .await
        .unwrap_err();
    assert_eq!(code(e), "frozen");
    let e = write(&s, &o, dec.num, &o.user, &format!("{DECISION}\nmore"))
        .await
        .unwrap_err();
    assert_eq!(code(e), "frozen");
    let e = s.split_doc(&w(&o, dec.num, &o.user), 1).await.unwrap_err();
    assert_eq!(code(e), "frozen");
    // The status still changes.
    set(
        &s,
        &o,
        dec.num,
        &o.a,
        DocChange {
            status: Some(St::Archived),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    // A team can make every kind wait for the user.
    s.update_seat(
        o.org.id,
        "b",
        &SeatUpdate {
            group: Some(Some("core".into())),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    s.put_team(
        o.org.id,
        "core",
        &TeamChange {
            doc_activate_by: Some(Some("human".into())),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let r = new(&s, &o, &o.b, "reference", "core doc", "x").await;
    assert_eq!(r.team.as_deref(), Some("core"));
    let e = set(
        &s,
        &o,
        r.num,
        &o.b,
        DocChange {
            status: Some(St::Active),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(code(e), "not_owner");
    set(
        &s,
        &o,
        r.num,
        &o.user,
        DocChange {
            status: Some(St::Active),
            ..Default::default()
        },
    )
    .await
    .unwrap();
}

async fn relations_and_supersession(s: S) {
    let o = setup(&s, "alpha").await;
    let top = new(&s, &o, &o.a, "reference", "top", "").await;
    let mut n = nd(&o, &o.a, "reference", "child", "");
    n.parent = Some(top.num);
    let child = s.create_doc(&n).await.unwrap().doc;
    assert_eq!(child.parent, Some(top.num));
    // No cycles; at most 8 levels below a top document.
    let e = set(
        &s,
        &o,
        top.num,
        &o.a,
        DocChange {
            parent: Some(Some(child.num)),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(code(e), "parent_cycle");
    let mut last = child.num;
    for i in 0..7 {
        let mut n = nd(&o, &o.a, "reference", &format!("level {i}"), "");
        n.parent = Some(last);
        last = s.create_doc(&n).await.unwrap().doc.num;
    }
    let mut n = nd(&o, &o.a, "reference", "too deep", "");
    n.parent = Some(last);
    assert_eq!(code(s.create_doc(&n).await.unwrap_err()), "parent_too_deep");
    // Supplements at creation; shown on both.
    let mut n = nd(&o, &o.b, "reference", "more", "");
    n.supplements = Some(top.num);
    let c = s.create_doc(&n).await.unwrap();
    assert_eq!(c.related_to.len(), 1);
    assert_eq!(c.related_to[0].1.num, top.num);
    let det = s.doc_detail(o.org.id, top.num, 20).await.unwrap().unwrap();
    assert_eq!(det.supplemented_by, [c.doc.num]);
    assert_eq!(det.children, [child.num]);
    // Supersession: status and links on both, in one call.
    let newer = new(&s, &o, &o.b, "reference", "newer", "").await;
    let e = s
        .supersede_doc(&w(&o, top.num, &o.b), newer.num)
        .await
        .unwrap_err();
    assert_eq!(code(e), "not_owner");
    let c = s
        .supersede_doc(&w(&o, top.num, &o.a), newer.num)
        .await
        .unwrap();
    assert_eq!(c.doc.status, St::Superseded);
    assert_eq!(c.doc.superseded_by, Some(newer.num));
    let n2 = s.doc(o.org.id, newer.num).await.unwrap().unwrap();
    assert_eq!(n2.replaces, [top.num]);
    let e = s
        .supersede_doc(&w(&o, top.num, &o.a), newer.num)
        .await
        .unwrap_err();
    assert_eq!(code(e), "invalid_transition");
    // Related, both ways.
    set(
        &s,
        &o,
        newer.num,
        &o.b,
        DocChange {
            related: Some(vec![child.num]),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        s.doc(o.org.id, child.num).await.unwrap().unwrap().related,
        [newer.num]
    );
}

async fn links_dangling_and_stale(s: S) {
    let o = setup(&s, "alpha").await;
    let target = new(&s, &o, &o.a, "reference", "Target", "# Steps\n\nx\n").await;
    let t = s
        .create_task(&NewTaskRec {
            org_id: o.org.id,
            reporter_id: o.user.id,
            title: "task one".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let text = format!(
        "# Links\n\nSee d{n}#steps and d{n}#gone, d99, [[d{n}]], [x](doc:d{n}), t{t}, @b, @nobody, `src/a.rs:3`, docs/b.md.\n",
        n = target.num,
        t = t.num
    );
    let mut n = nd(&o, &o.b, "reference", "Linker", &text);
    n.paths = vec![("src/a.rs".into(), false), ("docs/b.md".into(), true)];
    let c = s.create_doc(&n).await.unwrap();
    let dangling: Vec<String> = c
        .dangling
        .iter()
        .map(|l| format!("{}#{}", l.id, l.anchor.clone().unwrap_or_default()))
        .collect();
    assert_eq!(
        dangling,
        [format!("{}#gone", target.num), "99#".to_string()]
    );
    let (out, _) = s.doc_links(o.org.id, c.doc.num).await.unwrap().unwrap();
    let kinds: Vec<(&str, &str)> = out
        .iter()
        .map(|l| (l.kind.as_str(), l.id.as_str()))
        .collect();
    assert!(kinds.contains(&("task", "1")) && kinds.contains(&("seat", "b")));
    assert!(
        !kinds.iter().any(|k| k.1 == "nobody"),
        "only live seats: {kinds:?}"
    );
    let task_link = out.iter().find(|l| l.kind == "task").unwrap();
    assert_eq!(task_link.title_at_write.as_deref(), Some("task one"));
    let path = out.iter().find(|l| l.id == "src/a.rs").unwrap();
    assert!(path.stale && path.line == Some(3));
    assert!(!out.iter().find(|l| l.id == "docs/b.md").unwrap().stale);
    // Backlinks.
    let (_, inn) = s.doc_links(o.org.id, target.num).await.unwrap().unwrap();
    assert!(inn.iter().all(|l| l.src == c.doc.num) && !inn.is_empty());
    // The check lists the dangling and stale links of the team (all here).
    let problems = s.doc_check(o.org.id, None, 100).await.unwrap();
    assert_eq!(problems.iter().filter(|l| l.dangling).count(), 2);
    assert_eq!(problems.iter().filter(|l| l.stale).count(), 1);
    // A later revision of the target that adds the section heals the link;
    // a new document d99 heals the other once it exists.
    write(&s, &o, target.num, &o.a, "# Steps\n\nx\n\n# Gone\n\nback\n")
        .await
        .unwrap();
    let problems = s.doc_check(o.org.id, None, 100).await.unwrap();
    assert_eq!(problems.iter().filter(|l| l.dangling).count(), 1);
    // Renamed targets show the current title next to the one at write.
    set(
        &s,
        &o,
        target.num,
        &o.a,
        DocChange {
            title: Some("Target v2".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let (out, _) = s.doc_links(o.org.id, c.doc.num).await.unwrap().unwrap();
    let l = out
        .iter()
        .find(|l| l.kind == "doc" && l.num == Some(target.num))
        .unwrap();
    assert_eq!(l.title_at_write.as_deref(), Some("Target"));
    assert_eq!(l.title.as_deref(), Some("Target v2"));
}

async fn split_rewrites_links(s: S) {
    let o = setup(&s, "alpha").await;
    let big = new(
        &s,
        &o,
        &o.a,
        "runbook",
        "Ops",
        "Intro.\n\n# When\n\nalways\n\n# Steps\n\nrun\n\n## Rollback\n\nundo; see d1#when\n\n# Verify\n\ncheck\n",
    )
    .await;
    let other = new(
        &s,
        &o,
        &o.b,
        "reference",
        "Notes",
        "See d1#rollback and d1#verify and d1.\n",
    )
    .await;
    let t = s
        .create_task(&NewTaskRec {
            org_id: o.org.id,
            reporter_id: o.user.id,
            title: "do it".into(),
            body: "follow d1#steps".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(
        code(s.split_doc(&w(&o, big.num, &o.a), 3).await.unwrap_err()),
        "nothing_to_split"
    );
    let m = s.split_doc(&w(&o, big.num, &o.a), 1).await.unwrap();
    let nums: Vec<i64> = m.created.iter().map(|d| d.num).collect();
    assert_eq!(nums, [3, 4, 5]);
    assert_eq!(m.moved[1], ("#steps".to_string(), "d4".to_string()));
    assert!(
        m.created
            .iter()
            .all(|d| d.parent == Some(big.num) && d.kind == "runbook" && d.owner == "a")
    );
    assert_eq!(m.created[1].title, "Steps");
    let parent = body(&s, &o, big.num).await;
    assert_eq!(parent, "Intro.\n\n- d3 When\n- d4 Steps\n- d5 Verify\n");
    let steps = body(&s, &o, 4).await;
    assert!(steps.starts_with("run\n\n## Rollback"), "{steps}");
    // Links into moved sections now point at the parts, also from inside
    // a moved part, from other documents and from task bodies.
    assert!(steps.contains("see d3"), "{steps}");
    assert_eq!(
        body(&s, &o, other.num).await,
        "See d4#rollback and d5 and d1.\n"
    );
    assert_eq!(m.rewritten, [other.num]);
    assert_eq!(m.tasks_rewritten, [t.num]);
    assert_eq!(
        s.task(o.org.id, t.num).await.unwrap().unwrap().body,
        "follow d4"
    );
    // Each touched document has a revision and an event.
    let evs = s
        .doc_events(o.org.id, other.num, 0, 100)
        .await
        .unwrap()
        .unwrap();
    assert!(
        evs.iter()
            .any(|e| e.kind == K::LinksRewritten && e.rev == Some(2))
    );
    let evs = s
        .doc_events(o.org.id, big.num, 0, 100)
        .await
        .unwrap()
        .unwrap();
    assert!(evs.iter().any(|e| e.kind == K::Split));
    assert!(s.doc_check(o.org.id, None, 100).await.unwrap().is_empty());
}

async fn extract_and_merge(s: S) {
    let o = setup(&s, "alpha").await;
    let src = new(
        &s,
        &o,
        &o.a,
        "reference",
        "Guide",
        "# Setup\n\ninstall\n\n# Deploy\n\npush\n\n## Rollback\n\nundo\n\n# FAQ\n\nq\n",
    )
    .await;
    let other = new(
        &s,
        &o,
        &o.a,
        "reference",
        "Other",
        "see d1#rollback, d1#deploy\n",
    )
    .await;
    let m = s
        .extract_doc(&w(&o, src.num, &o.a), "deploy", "Deploying", None)
        .await
        .unwrap();
    let part = &m.created[0];
    assert_eq!(part.num, 3);
    assert_eq!(part.supplements, Some(src.num));
    assert_eq!(part.title, "Deploying");
    assert_eq!(
        body(&s, &o, part.num).await,
        "push\n\n## Rollback\n\nundo\n"
    );
    assert_eq!(
        body(&s, &o, src.num).await,
        "# Setup\n\ninstall\n\n# Deploy\n\nSee d3.\n\n# FAQ\n\nq\n"
    );
    assert_eq!(body(&s, &o, other.num).await, "see d3#rollback, d3\n");
    assert_eq!(
        code(
            s.extract_doc(&w(&o, src.num, &o.a), "nope", "x", None)
                .await
                .unwrap_err()
        ),
        "no_section"
    );
    // Merge it back: only the owner of both (or the user).
    let b_doc = new(&s, &o, &o.b, "reference", "B's", "").await;
    assert_eq!(
        code(
            s.merge_doc(&w(&o, part.num, &o.a), b_doc.num)
                .await
                .unwrap_err()
        ),
        "not_owner"
    );
    let m = s.merge_doc(&w(&o, part.num, &o.a), src.num).await.unwrap();
    let merged = body(&s, &o, src.num).await;
    assert!(
        merged.ends_with("# Deploying\n\npush\n\n## Rollback\n\nundo\n"),
        "{merged}"
    );
    assert_eq!(m.moved[0], ("d3".to_string(), "d1#deploying".to_string()));
    assert_eq!(
        body(&s, &o, other.num).await,
        "see d1#rollback, d1#deploying\n"
    );
    let gone = s.doc(o.org.id, part.num).await.unwrap().unwrap();
    assert_eq!(gone.status, St::Superseded);
    assert_eq!(gone.superseded_by, Some(src.num));
}

async fn search_by_section(s: S) {
    let o = setup(&s, "alpha").await;
    s.update_seat(
        o.org.id,
        "b",
        &SeatUpdate {
            group: Some(Some("core".into())),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let a1 = new(&s, &o, &o.a, "reference", "Deploy guide", "# Rollback\n\nRun the rollback script. Then check.\n\n# 部署说明\n\n先停止服务，再部署。\n").await;
    let b1 = new(
        &s,
        &o,
        &o.b,
        "reference",
        "Core rollback",
        "# Rollback\n\nThe core rollback differs.\n",
    )
    .await;
    let draft = new(
        &s,
        &o,
        &o.a,
        "reference",
        "Draft rollback",
        "rollback draft\n",
    )
    .await;
    for d in [a1.num, b1.num] {
        set(
            &s,
            &o,
            d,
            &o.user,
            DocChange {
                status: Some(St::Active),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }
    let q = |terms: &[&str]| DocSearchQuery {
        org_id: o.org.id,
        terms: terms.iter().map(|t| t.to_string()).collect(),
        limit: 20,
        now: amail_store::now_ms(),
        ..Default::default()
    };
    // Active only by default; the caller's team first.
    let r = s
        .search_docs(&DocSearchQuery {
            caller_team: s.doc(o.org.id, b1.num).await.unwrap().unwrap().team_id,
            ..q(&["ROLLBACK"])
        })
        .await
        .unwrap();
    assert!(!r.titles_only);
    let hits: Vec<(i64, &str)> = r
        .hits
        .iter()
        .map(|h| (h.doc.num, h.anchor.as_str()))
        .collect();
    assert_eq!(hits, [(b1.num, "rollback"), (a1.num, "rollback")]);
    assert!(r.hits[1].text.contains("Run the rollback script."));
    // All statuses.
    let r = s
        .search_docs(&DocSearchQuery {
            statuses: St::ALL.to_vec(),
            ..q(&["rollback"])
        })
        .await
        .unwrap();
    assert!(r.hits.iter().any(|h| h.doc.num == draft.num));
    // CJK of 3 characters and more: sections; two characters: titles and
    // descriptions only.
    let r = s.search_docs(&q(&["停止服务"])).await.unwrap();
    assert_eq!(r.hits.len(), 1);
    assert_eq!(r.hits[0].anchor, "部署说明");
    let r = s.search_docs(&q(&["部署"])).await.unwrap();
    assert!(r.titles_only && r.hits.is_empty());
    let r = s.search_docs(&q(&["de"])).await.unwrap();
    assert!(r.titles_only);
    assert_eq!(
        r.hits.iter().map(|h| h.doc.num).collect::<Vec<_>>(),
        [a1.num]
    );
    // A short term narrows long ones.
    let r = s.search_docs(&q(&["rollback", "核"])).await.unwrap();
    assert!(r.hits.is_empty());
    // Stale documents come after fresh ones.
    set(
        &s,
        &o,
        b1.num,
        &o.b,
        DocChange {
            expires_at: Some(Some(1)),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let r = s
        .search_docs(&DocSearchQuery {
            caller_team: s.doc(o.org.id, b1.num).await.unwrap().unwrap().team_id,
            ..q(&["rollback"])
        })
        .await
        .unwrap();
    assert_eq!(
        r.hits.iter().map(|h| h.doc.num).collect::<Vec<_>>(),
        [a1.num, b1.num]
    );
    // Kind and label filters.
    let r = s
        .search_docs(&DocSearchQuery {
            kind: Some("finding".into()),
            ..q(&["rollback"])
        })
        .await
        .unwrap();
    assert!(r.hits.is_empty());
    // Find: titles, slugs and descriptions.
    let found = s
        .docs(&DocQuery {
            org_id: o.org.id,
            terms: vec!["core".into()],
            limit: 10,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(found.iter().map(|d| d.num).collect::<Vec<_>>(), [b1.num]);
}

async fn index_order_and_scope(s: S) {
    let o = setup(&s, "alpha").await;
    s.update_seat(
        o.org.id,
        "b",
        &SeatUpdate {
            group: Some(Some("core".into())),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let core = s
        .doc(
            o.org.id,
            new(
                &s,
                &o,
                &o.b,
                "runbook",
                "core runbook",
                "# When\n# Steps\n# Verify\n",
            )
            .await
            .num,
        )
        .await
        .unwrap()
        .unwrap();
    let team = core.team_id;
    let org_wide = new(
        &s,
        &o,
        &o.a,
        "runbook",
        "org runbook",
        "# When\n# Steps\n# Verify\n",
    )
    .await;
    let pinned = new(&s, &o, &o.a, "index", "pinned index", "").await;
    let reference = new(&s, &o, &o.a, "reference", "not in index", "").await;
    let draft = new(&s, &o, &o.a, "index", "still a draft", "").await;
    for n in [core.num, org_wide.num, pinned.num, reference.num] {
        set(
            &s,
            &o,
            n,
            &o.user,
            DocChange {
                status: Some(St::Active),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }
    set(
        &s,
        &o,
        pinned.num,
        &o.a,
        DocChange {
            pinned: Some(true),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let (docs, total) = s
        .doc_index(o.org.id, team, amail_store::now_ms(), None)
        .await
        .unwrap();
    assert_eq!(
        docs.iter().map(|d| d.num).collect::<Vec<_>>(),
        [pinned.num, core.num, org_wide.num]
    );
    assert_eq!(total, 3);
    let _ = draft;
    // Without a team: org-wide documents only.
    let (docs, _) = s
        .doc_index(o.org.id, None, amail_store::now_ms(), None)
        .await
        .unwrap();
    assert_eq!(
        docs.iter().map(|d| d.num).collect::<Vec<_>>(),
        [pinned.num, org_wide.num]
    );
    // The scan bound.
    let (docs, total) = s
        .doc_index(o.org.id, team, amail_store::now_ms(), Some(2))
        .await
        .unwrap();
    assert_eq!((docs.len(), total), (2, 3));
}

async fn candidates_on_create(s: S) {
    let o = setup(&s, "alpha").await;
    let mut n = nd(&o, &o.a, "reference", "Deploy rollback procedure", "");
    n.slug = Some("runbook.deploy.rollback".into());
    let d1 = s.create_doc(&n).await.unwrap().doc;
    new(&s, &o, &o.a, "reference", "Database schema notes", "").await;
    new(
        &s,
        &o,
        &o.a,
        "runbook",
        "Deploy rollback procedure",
        "# When\n# Steps\n# Verify\n",
    )
    .await;
    let new_one = new(&s, &o, &o.b, "reference", "Rollback of a deploy", "").await;
    let c = s
        .doc_candidates(
            o.org.id,
            "reference",
            "Rollback of a deploy",
            "",
            None,
            new_one.num,
            None,
        )
        .await
        .unwrap();
    assert_eq!(c.iter().map(|x| x.0.num).collect::<Vec<_>>(), [d1.num]);
    assert!(c[0].1 >= amail_proto::limits::DOC_CANDIDATE_SIMILARITY);
    let c = s
        .doc_candidates(
            o.org.id,
            "reference",
            "Unrelated topic",
            "",
            Some("runbook.deploy.rollback.v2"),
            0,
            None,
        )
        .await
        .unwrap();
    assert_eq!(c.iter().map(|x| x.0.num).collect::<Vec<_>>(), [d1.num]);
}

async fn expiry_and_dangling_claims(s: S) {
    let o = setup(&s, "alpha").await;
    let f = new(&s, &o, &o.a, "finding", "F", FINDING).await;
    let c = s.verify_doc(&w(&o, f.num, &o.a), None).await.unwrap();
    let exp = c.doc.expires_at.unwrap();
    assert!((exp - c.doc.verified_at.unwrap() - 30 * 86_400_000).abs() < 1000);
    assert_eq!(c.doc.verified_by.as_deref(), Some("a"));
    assert_eq!(
        code(s.verify_doc(&w(&o, f.num, &o.b), None).await.unwrap_err()),
        "not_owner"
    );
    // Not due yet; due once; verifying again makes it due again later.
    assert!(
        s.claim_expired_docs(amail_store::now_ms(), 100)
            .await
            .unwrap()
            .is_empty()
    );
    let got = s.claim_expired_docs(exp + 1, 100).await.unwrap();
    assert_eq!(got.iter().map(|d| d.num).collect::<Vec<_>>(), [f.num]);
    assert!(s.claim_expired_docs(exp + 2, 100).await.unwrap().is_empty());
    let c = s
        .verify_doc(&w(&o, f.num, &o.a), Some(Some(1000)))
        .await
        .unwrap();
    let exp2 = c.doc.expires_at.unwrap();
    assert_eq!(s.claim_expired_docs(exp2 + 1, 100).await.unwrap().len(), 1);
    // No expiry.
    let c = s.verify_doc(&w(&o, f.num, &o.a), Some(None)).await.unwrap();
    assert_eq!(c.doc.expires_at, None);
    // Dangling links: once per day.
    let l = new(&s, &o, &o.a, "reference", "L", "see d77").await;
    let now = amail_store::now_ms();
    let got = s.claim_dangling_docs(now, 100).await.unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].0.num, l.num);
    assert_eq!(got[0].1[0].id, "77");
    assert!(
        s.claim_dangling_docs(now + 1000, 100)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        s.claim_dangling_docs(now + 25 * 3_600_000, 100)
            .await
            .unwrap()
            .len(),
        1
    );
    s.record_doc_reminder(o.org.id, l.id, "dangling", "a", 5)
        .await
        .unwrap();
    let evs = s
        .doc_events(o.org.id, l.num, 0, 100)
        .await
        .unwrap()
        .unwrap();
    assert!(
        evs.iter()
            .any(|e| e.kind == K::ReminderSent && e.field.as_deref() == Some("dangling"))
    );
}

async fn comments_on_removed_sections(s: S) {
    let o = setup(&s, "alpha").await;
    let d = new(
        &s,
        &o,
        &o.a,
        "reference",
        "R",
        "# Deploy\n\n## Rollback\n\nundo\n",
    )
    .await;
    s.record_doc_comment(
        &w(&o, d.num, &o.b),
        11,
        10,
        Some("rollback"),
        Some("Deploy > Rollback"),
    )
    .await
    .unwrap();
    s.record_doc_comment(&w(&o, d.num, &o.user), 12, 10, None, None)
        .await
        .unwrap();
    assert_eq!(
        s.doc_commenters(o.org.id, d.num).await.unwrap(),
        ["b", "owner"]
    );
    let det = s.doc_detail(o.org.id, d.num, 20).await.unwrap().unwrap();
    assert_eq!(det.doc.thread_id, Some(10));
    assert!(!det.comments[0].removed);
    write(&s, &o, d.num, &o.a, "# Deploy\n\nnew text\n")
        .await
        .unwrap();
    let det = s.doc_detail(o.org.id, d.num, 20).await.unwrap().unwrap();
    assert!(det.comments[0].removed);
    assert_eq!(det.comments[0].path.as_deref(), Some("Deploy > Rollback"));
    assert!(!det.comments[1].removed);
    // Threads citing it.
    assert!(
        s.add_doc_thread(&w(&o, d.num, &o.a), 20, "[d1]")
            .await
            .unwrap()
    );
    assert!(
        !s.add_doc_thread(&w(&o, d.num, &o.a), 20, "[d1]")
            .await
            .unwrap()
    );
    assert!(
        !s.add_doc_thread(&w(&o, d.num, &o.a), 10, "[d1]")
            .await
            .unwrap(),
        "its own thread"
    );
    assert!(
        !s.add_doc_thread(&w(&o, 99, &o.a), 20, "[d99]")
            .await
            .unwrap()
    );
    let docs = s.thread_docs(o.org.id, 20).await.unwrap();
    assert_eq!(docs.iter().map(|d| d.num).collect::<Vec<_>>(), [d.num]);
}

async fn quota_and_deletion(s: S) {
    let o = setup(&s, "alpha").await;
    let d = new(&s, &o, &o.a, "reference", "R", &"x".repeat(1000)).await;
    let org = s.org_by_id(o.org.id).await.unwrap().unwrap();
    assert!(org.doc_bytes > 1000, "{}", org.doc_bytes);
    let mut n = nd(&o, &o.a, "reference", "R2", &"y".repeat(5000));
    n.w.quota = Some(org.stored_bytes + org.task_bytes + org.doc_bytes + 100);
    assert!(matches!(
        s.create_doc(&n).await.unwrap_err(),
        StoreError::StorageQuota(_)
    ));
    let mut bw = w(&o, d.num, &o.a);
    bw.quota = Some(1);
    assert!(matches!(
        s.write_doc_body(
            &bw,
            &DocBodyRec {
                body: "z".repeat(3000),
                ..Default::default()
            }
        )
        .await
        .unwrap_err(),
        StoreError::StorageQuota(_)
    ));
    // Org deletion takes the documents.
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
    let t = new(&s, &o, &o.a, "reference", "team doc", "").await;
    assert!(t.team.is_some());
    assert!(s.delete_team(o.org.id, "core").await.unwrap());
    assert_eq!(s.doc(o.org.id, t.num).await.unwrap().unwrap().team, None);
    assert!(s.delete_org(o.org.id).await.unwrap());
    let o2 = setup(&s, "alpha").await;
    assert_eq!(new(&s, &o2, &o2.a, "reference", "fresh", "").await.num, 1);
}
