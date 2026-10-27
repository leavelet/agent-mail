//! Documents: the SQL shared by both backends, as `tasks.rs` does it for
//! tasks. [`impl_doc_queries!`] generates `mod dq` for one sqlx database
//! type; `impl_store_common!` invokes it and implements the document
//! methods of [`crate::Store`] on top of it.
//!
//! Every document write first takes the org's row (as task writes do), so
//! numbers, slugs, the parent check and the link rewriting of `split`,
//! `extract` and `merge` are decided on a state no other write changes
//! meanwhile. The body is parsed on every write ([`crate::docmd`]); the
//! sections and links of the current revision are stored for section reads,
//! search and backlinks.

use crate::docmd::{self, Parsed};
use crate::model::*;
use crate::{Result, StoreError};
use amail_proto::limits::*;

/// The built-in kinds: name, description, required sections, activated
/// by, in the index, verification interval in days.
/// A built-in kind (see [`BUILTIN_KINDS`]).
pub(crate) type BuiltinKind = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    bool,
    Option<i64>,
);

pub(crate) const BUILTIN_KINDS: &[BuiltinKind] = &[
    (
        "decision",
        "why something was decided; frozen once active",
        "Context,Decision,Alternatives,Consequences",
        "human",
        true,
        None,
    ),
    (
        "finding",
        "a verified fact about a system or harness, with the evidence",
        "Observed,Evidence,Applies to",
        "owner",
        true,
        Some(30),
    ),
    (
        "runbook",
        "a procedure",
        "When,Steps,Verify",
        "owner",
        true,
        Some(60),
    ),
    (
        "index",
        "a curated list of documents on a topic",
        "",
        "owner",
        true,
        Some(30),
    ),
    (
        "reference",
        "imported or long material; searchable, never injected",
        "",
        "owner",
        false,
        None,
    ),
];

/// Columns and joins of [`DocRow`] without the body.
pub(crate) const DOC_SELECT: &str = "SELECT d.id, d.org_id, d.num, d.slug, d.kind, d.title, d.description, '' AS body, \
     d.status, d.owner_id, o.name AS owner, d.team_id, tm.name AS team, d.labels, d.editors, d.pinned, d.rev, \
     d.body_bytes, d.tokens, d.sha256, d.verified_at, d.verified_by, d.expires_at, d.thread_id, d.event_seq, \
     d.created_at, d.updated_at FROM docs d JOIN seats o ON o.id = d.owner_id LEFT JOIN teams tm ON tm.id = d.team_id";

/// [`DOC_SELECT`] with the current revision's body.
pub(crate) const DOC_SELECT_BODY: &str = "SELECT d.id, d.org_id, d.num, d.slug, d.kind, d.title, d.description, \
     COALESCE(r.body, '') AS body, d.status, d.owner_id, o.name AS owner, d.team_id, tm.name AS team, d.labels, \
     d.editors, d.pinned, d.rev, d.body_bytes, d.tokens, d.sha256, d.verified_at, d.verified_by, d.expires_at, \
     d.thread_id, d.event_seq, d.created_at, d.updated_at FROM docs d JOIN seats o ON o.id = d.owner_id \
     LEFT JOIN teams tm ON tm.id = d.team_id LEFT JOIN doc_revisions r ON r.doc_id = d.id AND r.rev = d.rev";

/// The links of current revisions with their source's number, the
/// target's current title and whether they are dangling or stale.
pub(crate) const LINK_SELECT: &str = "SELECT s.num AS src, l.src_anchor, l.target_kind, l.target_id, l.target_num, \
     l.target_anchor, l.target_line, l.target_title_at_write, \
     CASE WHEN l.target_kind = 'doc' THEN t.title WHEN l.target_kind = 'task' THEN tk.title ELSE NULL END AS title, \
     CAST(CASE WHEN l.target_kind = 'doc' AND (t.id IS NULL OR (l.target_anchor IS NOT NULL AND NOT EXISTS \
       (SELECT 1 FROM doc_sections x WHERE x.doc_id = t.id AND x.anchor = l.target_anchor))) THEN 1 ELSE 0 END AS BIGINT) AS dangling, \
     CAST(CASE WHEN l.target_kind = 'path' AND l.path_exists = FALSE THEN 1 ELSE 0 END AS BIGINT) AS stale \
     FROM doc_links l JOIN docs s ON s.id = l.src_doc \
     LEFT JOIN docs t ON l.target_kind = 'doc' AND t.org_id = l.org_id AND t.num = l.target_num \
     LEFT JOIN tasks tk ON l.target_kind = 'task' AND tk.org_id = l.org_id AND tk.num = l.target_num";

/// True for link `l` (with `t` the target document) when it is dangling.
pub(crate) const DANGLING: &str = "l.target_kind = 'doc' AND (t.id IS NULL OR (l.target_anchor IS NOT NULL AND NOT EXISTS \
     (SELECT 1 FROM doc_sections x WHERE x.doc_id = t.id AND x.anchor = l.target_anchor)))";

pub(crate) const EVENT_SELECT: &str = "SELECT e.doc_id, e.seq, e.at, s.name AS actor, e.written_by, e.kind, e.field, \
     e.from_value, e.to_value, e.note, e.message_id, e.rev FROM doc_events e LEFT JOIN seats s ON s.id = e.actor_id";

#[derive(sqlx::FromRow)]
pub(crate) struct DocRow {
    id: i64,
    org_id: i64,
    num: i64,
    slug: Option<String>,
    kind: String,
    title: String,
    description: String,
    body: String,
    status: String,
    owner_id: i64,
    owner: String,
    team_id: Option<i64>,
    team: Option<String>,
    labels: String,
    editors: String,
    pinned: bool,
    rev: i64,
    body_bytes: i64,
    tokens: i64,
    sha256: String,
    verified_at: Option<i64>,
    verified_by: Option<String>,
    expires_at: Option<i64>,
    thread_id: Option<i64>,
    event_seq: i64,
    created_at: i64,
    updated_at: i64,
}

impl DocRow {
    pub(crate) fn into_rec(self) -> Result<DocRec> {
        Ok(DocRec {
            id: self.id,
            org_id: self.org_id,
            num: self.num,
            slug: self.slug,
            kind: self.kind,
            title: self.title,
            description: self.description,
            body: self.body,
            status: crate::common::parse::<DocStatus>(&self.status)?,
            owner_id: self.owner_id,
            owner: self.owner,
            team_id: self.team_id,
            team: self.team,
            labels: crate::tasks::labels_from_db(&self.labels),
            editors: crate::tasks::labels_from_db(&self.editors),
            pinned: self.pinned,
            rev: self.rev,
            body_bytes: self.body_bytes,
            tokens: self.tokens,
            sha256: self.sha256,
            verified_at: self.verified_at,
            verified_by: self.verified_by,
            expires_at: self.expires_at,
            thread_id: self.thread_id,
            event_seq: self.event_seq,
            created_at: self.created_at,
            updated_at: self.updated_at,
            parent: None,
            supplements: None,
            superseded_by: None,
            replaces: Vec::new(),
            related: Vec::new(),
        })
    }
}

#[derive(sqlx::FromRow)]
pub(crate) struct HitRow {
    #[sqlx(flatten)]
    doc: DocRow,
    hit_anchor: String,
    hit_path: String,
    hit_text: String,
}

impl HitRow {
    pub(crate) fn into_hit(self) -> Result<DocHitRec> {
        Ok(DocHitRec {
            doc: self.doc.into_rec()?,
            anchor: self.hit_anchor,
            path: self.hit_path,
            text: self.hit_text,
        })
    }
}

#[derive(sqlx::FromRow)]
pub(crate) struct KindRow {
    id: i64,
    org_id: i64,
    name: String,
    description: String,
    required_sections: String,
    activate_by: String,
    in_index: bool,
    body_cap_bytes: i64,
    verify_days: Option<i64>,
    builtin: bool,
    disabled: bool,
    docs: i64,
}

impl KindRow {
    pub(crate) fn into_rec(self) -> Result<DocKindRec> {
        Ok(DocKindRec {
            id: self.id,
            org_id: self.org_id,
            name: self.name,
            description: self.description,
            required_sections: self
                .required_sections
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
            activate_by: crate::common::parse::<ActivateBy>(&self.activate_by)?,
            in_index: self.in_index,
            body_cap_bytes: self.body_cap_bytes,
            verify_days: self.verify_days,
            builtin: self.builtin,
            disabled: self.disabled,
            docs: self.docs,
        })
    }
}

pub(crate) const KIND_SELECT: &str = "SELECT k.id, k.org_id, k.name, k.description, k.required_sections, k.activate_by, \
     k.in_index, k.body_cap_bytes, k.verify_days, k.builtin, k.disabled, \
     (SELECT COUNT(*) FROM docs x WHERE x.org_id = k.org_id AND x.kind = k.name) AS docs FROM doc_kinds k";

#[derive(sqlx::FromRow)]
pub(crate) struct LinkRow {
    src: i64,
    src_anchor: String,
    target_kind: String,
    target_id: String,
    target_num: Option<i64>,
    target_anchor: Option<String>,
    target_line: Option<i64>,
    target_title_at_write: Option<String>,
    title: Option<String>,
    dangling: i64,
    stale: i64,
}

impl LinkRow {
    pub(crate) fn into_rec(self) -> DocLinkRec {
        DocLinkRec {
            src: self.src,
            src_anchor: self.src_anchor,
            kind: self.target_kind,
            id: self.target_id,
            num: self.target_num,
            anchor: self.target_anchor,
            line: self.target_line,
            title_at_write: self.target_title_at_write,
            title: self.title,
            dangling: self.dangling != 0,
            stale: self.stale != 0,
        }
    }
}

#[derive(sqlx::FromRow)]
pub(crate) struct EventRow {
    doc_id: i64,
    seq: i64,
    at: i64,
    actor: Option<String>,
    written_by: Option<String>,
    kind: String,
    field: Option<String>,
    from_value: Option<String>,
    to_value: Option<String>,
    note: Option<String>,
    message_id: Option<i64>,
    rev: Option<i64>,
}

impl EventRow {
    pub(crate) fn into_rec(self) -> Result<DocEventRec> {
        Ok(DocEventRec {
            doc_id: self.doc_id,
            seq: self.seq,
            at: self.at,
            actor: self.actor,
            written_by: self.written_by,
            kind: crate::common::parse::<DocEventKind>(&self.kind)?,
            field: self.field,
            from: self.from_value,
            to: self.to_value,
            note: self.note,
            message_id: self.message_id,
            rev: self.rev,
        })
    }
}

/// One event to write.
#[derive(Debug, Clone)]
pub(crate) struct DEv {
    pub kind: DocEventKind,
    pub field: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub note: Option<String>,
    pub message_id: Option<i64>,
    pub rev: Option<i64>,
}

impl DEv {
    pub(crate) fn new(kind: DocEventKind) -> Self {
        DEv {
            kind,
            field: None,
            from: None,
            to: None,
            note: None,
            message_id: None,
            rev: None,
        }
    }
    pub(crate) fn field(mut self, f: &str) -> Self {
        self.field = Some(f.to_string());
        self
    }
    pub(crate) fn from(mut self, v: Option<String>) -> Self {
        self.from = v;
        self
    }
    pub(crate) fn to(mut self, v: Option<String>) -> Self {
        self.to = v;
        self
    }
    pub(crate) fn note(mut self, v: Option<String>) -> Self {
        self.note = v;
        self
    }
    pub(crate) fn bytes(&self, written_by: Option<&str>) -> i64 {
        STORED_BYTES_PER_DOC_ROW
            + [&self.field, &self.from, &self.to, &self.note]
                .iter()
                .map(|v| v.as_deref().map_or(0, str::len) as i64)
                .sum::<i64>()
            + written_by.map_or(0, str::len) as i64
    }
}

/// `d12`.
pub(crate) fn dnum(n: i64) -> String {
    format!("d{n}")
}

/// The bytes of a document's row (its fields).
pub(crate) fn doc_row_bytes(d: &DocRec) -> i64 {
    STORED_BYTES_PER_DOC
        + (d.title.len()
            + d.description.len()
            + d.slug.as_deref().map_or(0, str::len)
            + crate::tasks::labels_to_db(&d.labels).len()
            + crate::tasks::labels_to_db(&d.editors).len()
            + d.verified_by.as_deref().map_or(0, str::len)) as i64
}

/// The bytes of a revision.
pub(crate) fn rev_bytes(title: &str, description: &str, body: &str, message: Option<&str>) -> i64 {
    STORED_BYTES_PER_DOC_ROW
        + (title.len() + description.len() + body.len() + message.map_or(0, str::len) + 64) as i64
}

/// The bytes of a revision's sections and links as stored (within the
/// bounds).
pub(crate) fn index_bytes(p: &Parsed, body: &str, b: &DocBounds) -> i64 {
    let ns = b.sections_per_doc.unwrap_or(usize::MAX);
    let nl = b.links_per_rev.unwrap_or(usize::MAX);
    let s: i64 = p
        .sections
        .iter()
        .take(ns)
        .map(|s| {
            STORED_BYTES_PER_DOC_ROW
                + (s.own_end - s.start + s.path.len() + s.anchor.len() + s.heading.len()) as i64
        })
        .sum();
    let l: i64 = p
        .links
        .iter()
        .take(nl)
        .map(|l| {
            STORED_BYTES_PER_DOC_ROW + (l.id.len() + l.anchor.as_deref().map_or(0, str::len)) as i64
        })
        .sum();
    debug_assert!(p.sections.iter().all(|s| s.own_end <= body.len()));
    s + l
}

pub(crate) fn refused(code: &'static str, msg: impl Into<String>) -> StoreError {
    StoreError::Refused(code, msg.into())
}

/// Refuse a write with `stale` when the document is not at the revision
/// the writer expects.
pub(crate) fn check_rev(w: &DocWrite, d: &DocRec) -> Result<()> {
    match w.if_rev {
        Some(r) if r != d.rev => Err(refused(
            "stale",
            format!("d{} changed since rev {r}: it is at rev {}", d.num, d.rev),
        )),
        _ => Ok(()),
    }
}

/// The body of an active decision cannot change.
pub(crate) fn check_frozen(d: &DocRec) -> Result<()> {
    if d.kind == "decision" && d.status == DocStatus::Active {
        return Err(refused(
            "frozen",
            format!(
                "d{} is an active decision: its body cannot change; write a new decision and supersede this one (amail doc supersede d{} --by dN)",
                d.num, d.num
            ),
        ));
    }
    Ok(())
}

/// The body's size against the kind's cap and the hard cap.
pub(crate) fn check_size(body: &str, k: &DocKindRec) -> Result<()> {
    let cap = (k.body_cap_bytes.max(0) as usize).min(MAX_DOC_BODY_BYTES);
    if body.len() > cap {
        return Err(refused(
            "body_too_large",
            format!(
                "the body is {} bytes; documents of kind {} hold at most {} bytes: split it into parts (amail doc split) or move sections out (amail doc extract)",
                docmd::thousands(body.len() as i64),
                k.name,
                docmd::thousands(cap as i64)
            ),
        ));
    }
    Ok(())
}

pub(crate) fn check_required(p: &Parsed, k: &DocKindRec) -> Result<()> {
    let missing = docmd::missing_sections(p, &k.required_sections);
    if !missing.is_empty() {
        return Err(refused(
            "sections_missing",
            format!(
                "a {} needs the sections {}; missing: {}",
                k.name,
                k.required_sections.join(", "),
                missing.join(", ")
            ),
        ));
    }
    Ok(())
}

/// Replace the section `anchor` of `body` with `text`: the heading stays
/// unless `text` starts with a heading.
///
/// The spacing of the original is kept: the line breaks between the heading
/// and its text (a blank line when the section had no text), and those
/// between the section and what follows it.
pub(crate) fn replace_section(body: &str, p: &Parsed, anchor: &str, text: &str) -> Option<String> {
    let s = p.section(anchor)?;
    let old = &body[s.start..s.end];
    let mut out = String::with_capacity(body.len() + text.len());
    if s.level > 0 && !text.trim_start().starts_with('#') {
        out.push_str(&body[..s.heading_end]);
        let after = &body[s.heading_end..s.end];
        let gap = &after[..after.len() - after.trim_start_matches(['\n', '\r']).len()];
        if gap.len() == after.len() || gap.is_empty() {
            out.push_str("\n\n");
        } else {
            out.push_str(gap);
        }
    } else {
        out.push_str(&body[..s.start]);
    }
    out.push_str(text.trim_matches(['\n', '\r']));
    let tail = &old[old.trim_end_matches(['\n', '\r']).len()..];
    out.push_str(if tail.is_empty() { "\n\n" } else { tail });
    let rest = &body[s.end..];
    out.push_str(rest);
    let trimmed = out.trim_end_matches('\n').len();
    out.truncate(trimmed);
    out.push('\n');
    Some(out)
}

/// The anchors of `p`'s sections nested in `s` (by order) mapped to the
/// anchors of the level > 0 sections of `moved` (the same text parsed on
/// its own).
pub(crate) fn nested_map(p: &Parsed, s: &docmd::Section, moved: &Parsed) -> Vec<(String, String)> {
    let inner: Vec<&docmd::Section> = p
        .sections
        .iter()
        .filter(|x| x.level > 0 && x.start > s.start && x.end <= s.end)
        .collect();
    let new: Vec<&docmd::Section> = moved.sections.iter().filter(|x| x.level > 0).collect();
    inner
        .iter()
        .zip(new.iter())
        .map(|(a, b)| (a.anchor.clone(), b.anchor.clone()))
        .collect()
}

/// The text of a section without its heading line.
pub(crate) fn section_content(body: &str, s: &docmd::Section) -> String {
    let start = if s.level > 0 { s.heading_end } else { s.start };
    let t = body[start..s.end].trim_matches('\n');
    format!("{t}\n")
}

/// Generates `mod dq`: the document queries for one sqlx database type.
macro_rules! impl_doc_queries {
    ($db:ty, $dialect:expr) => {
        #[allow(dead_code)]
        pub(crate) mod dq {
            use $crate::common::{like_pattern, placeholders, Dialect};
            use $crate::docmd::{self, LinkKind, Parsed};
            use $crate::docs::*;
            use $crate::model::*;
            use $crate::tasks::labels_to_db;
            use $crate::{now_ms, Result, StoreError};
            use amail_proto::limits::*;
            use std::collections::{HashMap, HashSet};

            pub(crate) type Conn = <$db as sqlx::Database>::Connection;
            type Db = $db;
            const DIALECT: Dialect = $dialect;

            pub async fn lock(c: &mut Conn, org_id: i64) -> Result<()> {
                super::tq::lock(c, org_id).await
            }

            pub async fn insert_builtin_kinds(c: &mut Conn, org_id: i64) -> Result<()> {
                let now = now_ms();
                for (name, desc, req, by, in_index, days) in BUILTIN_KINDS {
                    sqlx::query(
                        "INSERT INTO doc_kinds (org_id, name, description, required_sections, activate_by, in_index, \
                         body_cap_bytes, verify_days, builtin, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, TRUE, $9) \
                         ON CONFLICT DO NOTHING",
                    )
                    .bind(org_id)
                    .bind(*name)
                    .bind(*desc)
                    .bind(*req)
                    .bind(*by)
                    .bind(*in_index)
                    .bind(MAX_DOC_BODY_BYTES as i64)
                    .bind(*days)
                    .bind(now)
                    .execute(&mut *c)
                    .await?;
                }
                Ok(())
            }

            /// Fill in the relations.
            pub async fn fill(c: &mut Conn, recs: &mut [DocRec]) -> Result<()> {
                for chunk in recs.chunks_mut(400) {
                    let ph = placeholders(1, chunk.len());
                    let sql = format!(
                        "SELECT r.src_id, r.kind, r.dst_id, s.num, t.num FROM doc_relations r \
                         JOIN docs s ON s.id = r.src_id JOIN docs t ON t.id = r.dst_id \
                         WHERE r.src_id IN ({ph}) OR r.dst_id IN ({ph2}) ORDER BY s.num, t.num",
                        ph2 = placeholders(1 + chunk.len(), chunk.len())
                    );
                    let mut q = sqlx::query_as::<Db, (i64, String, i64, i64, i64)>(&sql);
                    for d in chunk.iter() {
                        q = q.bind(d.id);
                    }
                    for d in chunk.iter() {
                        q = q.bind(d.id);
                    }
                    let rows = q.fetch_all(&mut *c).await?;
                    for d in chunk.iter_mut() {
                        d.replaces.clear();
                        d.related.clear();
                        for (src, kind, dst, sn, tn) in &rows {
                            match kind.as_str() {
                                "parent" if *src == d.id => d.parent = Some(*tn),
                                "supplements" if *src == d.id => d.supplements = Some(*tn),
                                "supersedes" if *src == d.id => d.replaces.push(*tn),
                                "supersedes" if *dst == d.id => d.superseded_by = Some(*sn),
                                "related" if *src == d.id => d.related.push(*tn),
                                "related" if *dst == d.id => d.related.push(*sn),
                                _ => {}
                            }
                        }
                        d.related.sort_unstable();
                        d.related.dedup();
                    }
                }
                Ok(())
            }

            pub async fn by_num(c: &mut Conn, org_id: i64, num: i64) -> Result<Option<DocRec>> {
                let sql = format!("{DOC_SELECT_BODY} WHERE d.org_id = $1 AND d.num = $2");
                let r: Option<DocRow> = sqlx::query_as(&sql).bind(org_id).bind(num).fetch_optional(&mut *c).await?;
                let Some(r) = r else { return Ok(None) };
                let mut v = [r.into_rec()?];
                fill(c, &mut v).await?;
                let [d] = v;
                Ok(Some(d))
            }

            pub async fn must(c: &mut Conn, org_id: i64, num: i64) -> Result<DocRec> {
                by_num(c, org_id, num).await?.ok_or_else(|| StoreError::NotFound(format!("no document d{num}")))
            }

            /// Documents by id (without bodies), by number.
            pub async fn by_ids(c: &mut Conn, ids: &[i64]) -> Result<Vec<DocRec>> {
                let mut out = Vec::new();
                for chunk in ids.chunks(500) {
                    let sql = format!("{DOC_SELECT} WHERE d.id IN ({})", placeholders(1, chunk.len()));
                    let mut q = sqlx::query_as::<Db, DocRow>(&sql);
                    for id in chunk {
                        q = q.bind(*id);
                    }
                    for r in q.fetch_all(&mut *c).await? {
                        out.push(r.into_rec()?);
                    }
                }
                out.sort_by_key(|d| d.num);
                out.dedup_by_key(|d| d.num);
                fill(c, &mut out).await?;
                Ok(out)
            }

            pub async fn num_by_slug(c: &mut Conn, org_id: i64, slug: &str) -> Result<Option<i64>> {
                Ok(sqlx::query_scalar("SELECT num FROM docs WHERE org_id = $1 AND slug = $2")
                    .bind(org_id)
                    .bind(slug)
                    .fetch_optional(&mut *c)
                    .await?)
            }

            pub async fn kind(c: &mut Conn, org_id: i64, name: &str) -> Result<Option<DocKindRec>> {
                let sql = format!("{KIND_SELECT} WHERE k.org_id = $1 AND k.name = $2");
                let r: Option<KindRow> = sqlx::query_as(&sql).bind(org_id).bind(name).fetch_optional(&mut *c).await?;
                r.map(KindRow::into_rec).transpose()
            }

            async fn must_kind(c: &mut Conn, org_id: i64, name: &str) -> Result<DocKindRec> {
                kind(c, org_id, name)
                    .await?
                    .ok_or_else(|| refused("unknown_kind", format!("no document kind {name:?} (amail doc kinds)")))
            }

            pub async fn kinds(c: &mut Conn, org_id: i64) -> Result<Vec<DocKindRec>> {
                let sql = format!("{KIND_SELECT} WHERE k.org_id = $1 ORDER BY k.builtin DESC, k.id");
                let rows: Vec<KindRow> = sqlx::query_as(&sql).bind(org_id).fetch_all(&mut *c).await?;
                rows.into_iter().map(KindRow::into_rec).collect()
            }

            pub async fn put_kind(c: &mut Conn, org_id: i64, k: &DocKindPut, max: Option<i64>) -> Result<DocKindRec> {
                lock(c, org_id).await?;
                let old = kind(c, org_id, &k.name).await?;
                let sections = k.required_sections.as_ref().map(|v| v.join(","));
                match old {
                    Some(o) => {
                        sqlx::query(
                            "UPDATE doc_kinds SET description = $2, required_sections = $3, activate_by = $4, in_index = $5, \
                             body_cap_bytes = $6, verify_days = $7, disabled = $8 WHERE id = $1",
                        )
                        .bind(o.id)
                        .bind(k.description.clone().unwrap_or(o.description.clone()))
                        .bind(sections.unwrap_or(o.required_sections.join(",")))
                        .bind(k.activate_by.unwrap_or(o.activate_by).as_str())
                        .bind(k.in_index.unwrap_or(o.in_index))
                        .bind(k.body_cap_bytes.unwrap_or(o.body_cap_bytes))
                        .bind(k.verify_days.unwrap_or(o.verify_days))
                        .bind(k.disabled.unwrap_or(o.disabled))
                        .execute(&mut *c)
                        .await?;
                    }
                    None => {
                        if let Some(max) = max {
                            let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM doc_kinds WHERE org_id = $1")
                                .bind(org_id)
                                .fetch_one(&mut *c)
                                .await?;
                            if n >= max {
                                return Err(refused("too_many_kinds", format!("an org has at most {max} document kinds on this server")));
                            }
                        }
                        sqlx::query(
                            "INSERT INTO doc_kinds (org_id, name, description, required_sections, activate_by, in_index, \
                             body_cap_bytes, verify_days, builtin, disabled, created_at) \
                             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, FALSE, $9, $10)",
                        )
                        .bind(org_id)
                        .bind(&k.name)
                        .bind(k.description.clone().unwrap_or_default())
                        .bind(sections.unwrap_or_default())
                        .bind(k.activate_by.unwrap_or(ActivateBy::Owner).as_str())
                        .bind(k.in_index.unwrap_or(true))
                        .bind(k.body_cap_bytes.unwrap_or(MAX_DOC_BODY_BYTES as i64))
                        .bind(k.verify_days.flatten())
                        .bind(k.disabled.unwrap_or(false))
                        .bind(now_ms())
                        .execute(&mut *c)
                        .await?;
                    }
                }
                must_kind(c, org_id, &k.name).await
            }

            /// Write one event; returns the bytes it adds.
            pub async fn event(c: &mut Conn, doc_id: i64, org_id: i64, actor: Option<i64>, written_by: Option<&str>, at: i64, e: DEv) -> Result<i64> {
                let seq: i64 = sqlx::query_scalar("UPDATE docs SET event_seq = event_seq + 1 WHERE id = $1 RETURNING event_seq")
                    .bind(doc_id)
                    .fetch_one(&mut *c)
                    .await?;
                let bytes = e.bytes(written_by);
                sqlx::query(
                    "INSERT INTO doc_events (doc_id, seq, org_id, at, actor_id, written_by, kind, field, from_value, \
                     to_value, note, message_id, rev) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
                )
                .bind(doc_id)
                .bind(seq)
                .bind(org_id)
                .bind(at)
                .bind(actor)
                .bind(written_by)
                .bind(e.kind.as_str())
                .bind(&e.field)
                .bind(&e.from)
                .bind(&e.to)
                .bind(&e.note)
                .bind(e.message_id)
                .bind(e.rev)
                .execute(&mut *c)
                .await?;
                Ok(bytes)
            }

            /// Add `delta` to the org's document bytes; growth is refused when
            /// the org's mail, task and document bytes would exceed `quota`.
            pub async fn charge(c: &mut Conn, org_id: i64, delta: i64, quota: Option<i64>) -> Result<()> {
                if delta == 0 {
                    return Ok(());
                }
                let ok: Option<i64> = sqlx::query_scalar(
                    "UPDATE orgs SET doc_bytes = doc_bytes + $1 WHERE id = $2 \
                     AND ($1 <= 0 OR CAST($3 AS BIGINT) IS NULL OR stored_bytes + task_bytes + doc_bytes + $1 <= $3) RETURNING id",
                )
                .bind(delta)
                .bind(org_id)
                .bind(quota)
                .fetch_optional(&mut *c)
                .await?;
                if ok.is_none() {
                    let stored: i64 = sqlx::query_scalar("SELECT stored_bytes + task_bytes + doc_bytes FROM orgs WHERE id = $1")
                        .bind(org_id)
                        .fetch_one(&mut *c)
                        .await?;
                    return Err(StoreError::StorageQuota(stored));
                }
                Ok(())
            }

            async fn seat_name(c: &mut Conn, id: Option<i64>) -> Result<Option<String>> {
                let Some(id) = id else { return Ok(None) };
                Ok(sqlx::query_scalar("SELECT name FROM seats WHERE id = $1").bind(id).fetch_optional(&mut *c).await?)
            }

            async fn team_human(c: &mut Conn, team: Option<i64>) -> Result<bool> {
                let Some(t) = team else { return Ok(false) };
                let v: Option<Option<String>> = sqlx::query_scalar("SELECT doc_activate_by FROM teams WHERE id = $1")
                    .bind(t)
                    .fetch_optional(&mut *c)
                    .await?;
                Ok(v.flatten().as_deref() == Some("human"))
            }

            /// The actor may revise the body, title, description, labels,
            /// slug, team and relations: the user, the owner, an editor.
            async fn can_revise(c: &mut Conn, w: &DocWrite, d: &DocRec) -> Result<()> {
                if w.is_user || w.actor_id == Some(d.owner_id) {
                    return Ok(());
                }
                if let Some(n) = seat_name(c, w.actor_id).await?
                    && d.editors.contains(&n)
                {
                    return Ok(());
                }
                Err(refused(
                    "not_owner",
                    format!(
                        "d{} is owned by {}; only the owner, the user and the editors the owner names may change it; comment instead (amail doc comment d{})",
                        d.num, d.owner, d.num
                    ),
                ))
            }

            /// The actor may change status, owner, editors, pinning,
            /// expiry and verification: the user and the owner.
            fn can_manage(w: &DocWrite, d: &DocRec, what: &str) -> Result<()> {
                if w.is_user || w.actor_id == Some(d.owner_id) {
                    return Ok(());
                }
                Err(refused(
                    "not_owner",
                    format!("only the owner of d{} ({}) and the user may change its {what}", d.num, d.owner),
                ))
            }

            /// Ancestors of document `id` (itself not included), nearest
            /// first, at most `MAX_DOC_PARENT_DEPTH + 1`.
            async fn ancestors(c: &mut Conn, id: i64) -> Result<Vec<i64>> {
                let mut out = Vec::new();
                let mut cur = id;
                while out.len() <= MAX_DOC_PARENT_DEPTH {
                    let p: Option<i64> = sqlx::query_scalar("SELECT dst_id FROM doc_relations WHERE src_id = $1 AND kind = 'parent'")
                        .bind(cur)
                        .fetch_optional(&mut *c)
                        .await?;
                    match p {
                        Some(p) if !out.contains(&p) => {
                            out.push(p);
                            cur = p;
                        }
                        _ => break,
                    }
                }
                Ok(out)
            }

            async fn height(c: &mut Conn, id: i64) -> Result<usize> {
                let mut level = vec![id];
                let mut h = 0;
                while !level.is_empty() && h <= MAX_DOC_PARENT_DEPTH {
                    let sql = format!(
                        "SELECT src_id FROM doc_relations WHERE kind = 'parent' AND dst_id IN ({})",
                        placeholders(1, level.len())
                    );
                    let mut q = sqlx::query_scalar::<Db, i64>(&sql);
                    for x in &level {
                        q = q.bind(*x);
                    }
                    level = q.fetch_all(&mut *c).await?;
                    if !level.is_empty() {
                        h += 1;
                    }
                }
                Ok(h)
            }

            async fn check_parent(c: &mut Conn, id: Option<i64>, parent: &DocRec) -> Result<()> {
                let up = ancestors(c, parent.id).await?;
                if let Some(id) = id
                    && (parent.id == id || up.contains(&id))
                {
                    return Err(refused("parent_cycle", format!("d{} is below this document; parents form a tree", parent.num)));
                }
                let below = match id {
                    Some(id) => height(c, id).await?,
                    None => 0,
                };
                if up.len() + 1 + below > MAX_DOC_PARENT_DEPTH {
                    return Err(refused(
                        "parent_too_deep",
                        format!("documents nest at most {MAX_DOC_PARENT_DEPTH} levels below a top document"),
                    ));
                }
                Ok(())
            }

            /// Set the single relation `kind` of `src` (parent,
            /// supplements) to `dst`; returns the bytes.
            async fn set_relation(c: &mut Conn, org_id: i64, src: i64, kind: &str, dst: Option<i64>, now: i64) -> Result<i64> {
                let n = sqlx::query("DELETE FROM doc_relations WHERE src_id = $1 AND kind = $2")
                    .bind(src)
                    .bind(kind)
                    .execute(&mut *c)
                    .await?
                    .rows_affected() as i64;
                let mut bytes = -n * STORED_BYTES_PER_DOC_ROW;
                if let Some(dst) = dst {
                    add_relation(c, org_id, src, kind, dst, now).await?;
                    bytes += STORED_BYTES_PER_DOC_ROW;
                }
                Ok(bytes)
            }

            async fn add_relation(c: &mut Conn, org_id: i64, src: i64, kind: &str, dst: i64, now: i64) -> Result<bool> {
                let n = sqlx::query(
                    "INSERT INTO doc_relations (src_id, kind, dst_id, org_id, created_at) VALUES ($1, $2, $3, $4, $5) \
                     ON CONFLICT DO NOTHING",
                )
                .bind(src)
                .bind(kind)
                .bind(dst)
                .bind(org_id)
                .bind(now)
                .execute(&mut *c)
                .await?
                .rows_affected();
                Ok(n > 0)
            }

            /// Replace the sections and links of document `d` with those of
            /// `body` (revision `rev`).
            async fn write_index(c: &mut Conn, d: &DocRec, rev: i64, body: &str, p: &Parsed, b: &DocBounds, paths: &[(String, bool)]) -> Result<()> {
                // What was known of paths stays known.
                let known: Vec<(String, Option<bool>)> = sqlx::query_as(
                    "SELECT target_id, path_exists FROM doc_links WHERE src_doc = $1 AND target_kind = 'path'",
                )
                .bind(d.id)
                .fetch_all(&mut *c)
                .await?;
                let mut exists: HashMap<String, bool> =
                    known.into_iter().filter_map(|(p, e)| e.map(|e| (p, e))).collect();
                for (p, e) in paths {
                    exists.insert(p.trim_start_matches("./").to_string(), *e);
                }
                sqlx::query("DELETE FROM doc_sections WHERE doc_id = $1").bind(d.id).execute(&mut *c).await?;
                sqlx::query("DELETE FROM doc_links WHERE src_doc = $1").bind(d.id).execute(&mut *c).await?;
                let ns = b.sections_per_doc.unwrap_or(usize::MAX);
                for (i, s) in p.sections.iter().take(ns).enumerate() {
                    sqlx::query(
                        "INSERT INTO doc_sections (doc_id, org_id, rev, ord, level, anchor, heading, path, start_byte, \
                         end_byte, tokens, text) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
                    )
                    .bind(d.id)
                    .bind(d.org_id)
                    .bind(rev)
                    .bind(i as i64)
                    .bind(s.level)
                    .bind(&s.anchor)
                    .bind(&s.heading)
                    .bind(&s.path)
                    .bind(s.start as i64)
                    .bind(s.end as i64)
                    .bind(s.tokens)
                    .bind(&body[s.start..s.own_end])
                    .execute(&mut *c)
                    .await?;
                }
                let nl = b.links_per_rev.unwrap_or(usize::MAX);
                let mut seats: HashMap<String, bool> = HashMap::new();
                for l in p.links.iter().take(nl) {
                    let title: Option<String> = match l.kind {
                        LinkKind::Doc => {
                            if l.num == Some(d.num) {
                                Some(d.title.clone())
                            } else {
                                sqlx::query_scalar("SELECT title FROM docs WHERE org_id = $1 AND num = $2")
                                    .bind(d.org_id)
                                    .bind(l.num)
                                    .fetch_optional(&mut *c)
                                    .await?
                            }
                        }
                        LinkKind::Task => sqlx::query_scalar("SELECT title FROM tasks WHERE org_id = $1 AND num = $2")
                            .bind(d.org_id)
                            .bind(l.num)
                            .fetch_optional(&mut *c)
                            .await?,
                        LinkKind::Message | LinkKind::Thread => sqlx::query_scalar(
                            "SELECT m.subject FROM messages m WHERE m.id = $2 AND (m.org_id = $1 OR EXISTS \
                             (SELECT 1 FROM deliveries x WHERE x.message_id = m.id AND x.org_id = $1))",
                        )
                        .bind(d.org_id)
                        .bind(l.num)
                        .fetch_optional(&mut *c)
                        .await?
                        .map(|s: String| amail_proto::text::line(&s).into_owned()),
                        LinkKind::Seat => {
                            let live = match seats.get(&l.id) {
                                Some(v) => *v,
                                None => {
                                    let v: Option<i64> = sqlx::query_scalar(
                                        "SELECT id FROM seats WHERE org_id = $1 AND name = $2 AND deleted_at IS NULL",
                                    )
                                    .bind(d.org_id)
                                    .bind(&l.id)
                                    .fetch_optional(&mut *c)
                                    .await?;
                                    seats.insert(l.id.clone(), v.is_some());
                                    v.is_some()
                                }
                            };
                            if !live {
                                continue;
                            }
                            None
                        }
                        LinkKind::Path => None,
                    };
                    sqlx::query(
                        "INSERT INTO doc_links (org_id, src_doc, src_rev, src_anchor, target_kind, target_id, target_num, \
                         target_anchor, target_line, target_title_at_write, path_exists) \
                         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
                    )
                    .bind(d.org_id)
                    .bind(d.id)
                    .bind(rev)
                    .bind(p.anchor_at(l.offset))
                    .bind(l.kind.as_str())
                    .bind(&l.id)
                    .bind(l.num)
                    .bind(&l.anchor)
                    .bind(l.line)
                    .bind(title)
                    .bind(if l.kind == LinkKind::Path { exists.get(&l.id).copied() } else { None })
                    .execute(&mut *c)
                    .await?;
                }
                Ok(())
            }

            /// Write a revision of `d` (which carries its current body) with
            /// this title, description and body; nothing when all three are
            /// unchanged. Returns the bytes it adds and whether it wrote.
            #[allow(clippy::too_many_arguments)]
            async fn revise(
                c: &mut Conn,
                w: &DocWrite,
                d: &DocRec,
                title: &str,
                description: &str,
                body: &str,
                message: Option<&str>,
                ev: DEv,
                paths: &[(String, bool)],
                now: i64,
            ) -> Result<(i64, bool)> {
                let sha = docmd::sha256_hex(body);
                if d.rev > 0 && sha == d.sha256 && title == d.title && description == d.description {
                    return Ok((0, false));
                }
                let p = docmd::parse(body);
                let rev = d.rev + 1;
                sqlx::query(
                    "INSERT INTO doc_revisions (doc_id, rev, org_id, title, description, body, sha256, message, author_id, \
                     written_by, at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
                )
                .bind(d.id)
                .bind(rev)
                .bind(d.org_id)
                .bind(title)
                .bind(description)
                .bind(body)
                .bind(&sha)
                .bind(message)
                .bind(w.actor_id)
                .bind(&w.written_by)
                .bind(now)
                .execute(&mut *c)
                .await?;
                sqlx::query(
                    "UPDATE docs SET title = $2, description = $3, rev = $4, body_bytes = $5, tokens = $6, sha256 = $7, \
                     updated_at = $8 WHERE id = $1",
                )
                .bind(d.id)
                .bind(title)
                .bind(description)
                .bind(rev)
                .bind(body.len() as i64)
                .bind(p.tokens)
                .bind(&sha)
                .bind(now)
                .execute(&mut *c)
                .await?;
                let mut bytes = rev_bytes(title, description, body, message)
                    + (title.len() as i64 - d.title.len() as i64)
                    + (description.len() as i64 - d.description.len() as i64);
                if d.rev > 0 {
                    bytes -= index_bytes(&docmd::parse(&d.body), &d.body, &w.bounds);
                }
                bytes += index_bytes(&p, body, &w.bounds);
                let mut d2 = d.clone();
                d2.title = title.to_string();
                write_index(c, &d2, rev, body, &p, &w.bounds, paths).await?;
                let mut parts = Vec::new();
                if d.rev > 0 {
                    if sha != d.sha256 {
                        parts.push("body");
                    }
                    if title != d.title {
                        parts.push("title");
                    }
                    if description != d.description {
                        parts.push("description");
                    }
                }
                let mut e = ev;
                e.rev = Some(rev);
                if e.field.is_none() && !parts.is_empty() {
                    e.field = Some(parts.join(","));
                }
                if e.note.is_none() {
                    e.note = message.map(str::to_string);
                }
                if title != d.title && d.rev > 0 && e.from.is_none() {
                    e.from = Some(d.title.clone());
                    e.to = Some(title.to_string());
                }
                bytes += event(c, d.id, w.org_id, w.actor_id, w.written_by.as_deref(), now, e).await?;
                Ok((bytes, true))
            }

            /// The dangling links of document `id`.
            pub async fn dangling_of(c: &mut Conn, id: i64) -> Result<Vec<DocLinkRec>> {
                let sql = format!("{LINK_SELECT} WHERE l.src_doc = $1 AND {DANGLING} ORDER BY l.id");
                let rows: Vec<LinkRow> = sqlx::query_as(&sql).bind(id).fetch_all(&mut *c).await?;
                Ok(rows.into_iter().map(LinkRow::into_rec).collect())
            }

            async fn check_slug(c: &mut Conn, org_id: i64, slug: &str, except: Option<i64>) -> Result<()> {
                let n: Option<i64> = sqlx::query_scalar("SELECT num FROM docs WHERE org_id = $1 AND slug = $2")
                    .bind(org_id)
                    .bind(slug)
                    .fetch_optional(&mut *c)
                    .await?;
                match n {
                    Some(n) if Some(n) != except => {
                        Err(refused("slug_taken", format!("the slug {slug:?} names d{n}")))
                    }
                    _ => Ok(()),
                }
            }

            fn changed(before: DocRec, doc: DocRec) -> DocChanged {
                DocChanged {
                    before,
                    doc,
                    fields: Vec::new(),
                    revised: false,
                    unchanged: false,
                    dangling: Vec::new(),
                    related_to: Vec::new(),
                    existing: false,
                }
            }

            pub async fn create(c: &mut Conn, n: &NewDocRec) -> Result<DocChanged> {
                let w = &n.w;
                let now = now_ms();
                lock(c, w.org_id).await?;
                if let Some(k) = &n.client_key {
                    let num: Option<i64> = sqlx::query_scalar(
                        "SELECT num FROM docs WHERE org_id = $1 AND client_key = $2 AND created_at >= $3 ORDER BY num DESC LIMIT 1",
                    )
                    .bind(w.org_id)
                    .bind(k)
                    .bind(now - TASK_CLIENT_KEY_HOURS * 3_600_000)
                    .fetch_optional(&mut *c)
                    .await?;
                    if let Some(num) = num {
                        let d = must(c, w.org_id, num).await?;
                        let mut ch = changed(d.clone(), d);
                        ch.existing = true;
                        return Ok(ch);
                    }
                }
                let k = must_kind(c, w.org_id, &n.kind).await?;
                if k.disabled {
                    return Err(refused("kind_disabled", format!("the kind {} takes no new documents", k.name)));
                }
                check_size(&n.body, &k)?;
                check_required(&docmd::parse(&n.body), &k)?;
                if let Some(s) = &n.slug {
                    check_slug(c, w.org_id, s, None).await?;
                }
                let num: Option<i64> = sqlx::query_scalar(
                    "UPDATE orgs SET org_doc_seq = org_doc_seq + 1 WHERE id = $1 AND deleted_at IS NULL RETURNING org_doc_seq",
                )
                .bind(w.org_id)
                .fetch_optional(&mut *c)
                .await?;
                let num = num.ok_or_else(|| StoreError::NotFound("org".into()))?;
                if let Some(max) = w.bounds.docs_per_org
                    && num > max
                {
                    return Err(refused("too_many_docs", format!("an org has at most {max} documents on this server")));
                }
                let team_id = match n.team_id {
                    Some(t) => Some(t),
                    None => {
                        let t: Option<Option<i64>> = sqlx::query_scalar("SELECT team_id FROM seats WHERE id = $1")
                            .bind(n.owner_id)
                            .fetch_optional(&mut *c)
                            .await?;
                        t.flatten()
                    }
                };
                let parent = match n.parent {
                    Some(p) => {
                        let p = must(c, w.org_id, p).await?;
                        check_parent(c, None, &p).await?;
                        Some(p)
                    }
                    None => None,
                };
                let supplements = match n.supplements {
                    Some(s) => Some(must(c, w.org_id, s).await?),
                    None => None,
                };
                let id: i64 = sqlx::query_scalar(
                    "INSERT INTO docs (org_id, num, slug, kind, title, description, status, owner_id, team_id, labels, \
                     editors, created_at, updated_at, client_key) \
                     VALUES ($1, $2, $3, $4, $5, $6, 'draft', $7, $8, $9, $10, $11, $11, $12) RETURNING id",
                )
                .bind(w.org_id)
                .bind(num)
                .bind(&n.slug)
                .bind(&k.name)
                .bind(&n.title)
                .bind(&n.description)
                .bind(n.owner_id)
                .bind(team_id)
                .bind(labels_to_db(&n.labels))
                .bind(labels_to_db(&n.editors))
                .bind(now)
                .bind(&n.client_key)
                .fetch_one(&mut *c)
                .await?;
                let blank = must(c, w.org_id, num).await?;
                let mut bytes = doc_row_bytes(&blank);
                let ev = DEv::new(DocEventKind::Created).to(Some(n.title.clone()));
                let (b, _) = revise(c, w, &blank, &n.title, &n.description, &n.body, n.message.as_deref(), ev, &n.paths, now).await?;
                bytes += b;
                let wb = w.written_by.as_deref();
                let mut related_to = Vec::new();
                if let Some(p) = &parent {
                    bytes += set_relation(c, w.org_id, id, "parent", Some(p.id), now).await?;
                    let e = DEv::new(DocEventKind::Relation).field("parent").to(Some(dnum(p.num)));
                    bytes += event(c, id, w.org_id, w.actor_id, wb, now, e).await?;
                }
                if let Some(s) = &supplements {
                    bytes += set_relation(c, w.org_id, id, "supplements", Some(s.id), now).await?;
                    let e = DEv::new(DocEventKind::Relation).field("supplements").to(Some(dnum(s.num)));
                    bytes += event(c, id, w.org_id, w.actor_id, wb, now, e).await?;
                    let e = DEv::new(DocEventKind::Relation).field("supplemented_by").to(Some(dnum(num)));
                    bytes += event(c, s.id, w.org_id, w.actor_id, wb, now, e).await?;
                    related_to.push(("supplements".to_string(), s.clone()));
                }
                charge(c, w.org_id, bytes, w.quota).await?;
                let d = must(c, w.org_id, num).await?;
                let mut ch = changed(d.clone(), d);
                ch.revised = true;
                ch.dangling = dangling_of(c, id).await?;
                ch.related_to = related_to;
                Ok(ch)
            }

            /// Replace the body or one section.
            pub async fn write_body(c: &mut Conn, w: &DocWrite, b: &DocBodyRec) -> Result<DocChanged> {
                lock(c, w.org_id).await?;
                let now = now_ms();
                let d = must(c, w.org_id, w.num).await?;
                check_rev(w, &d)?;
                can_revise(c, w, &d).await?;
                check_frozen(&d)?;
                let body = match &b.section {
                    Some(a) => {
                        let p = docmd::parse(&d.body);
                        replace_section(&d.body, &p, a, &b.body)
                            .ok_or_else(|| refused("no_section", format!("d{} has no section #{a} (amail doc read d{} --sections)", d.num, d.num)))?
                    }
                    None => b.body.clone(),
                };
                let k = must_kind(c, w.org_id, &d.kind).await?;
                check_size(&body, &k)?;
                check_required(&docmd::parse(&body), &k)?;
                let (bytes, revised) =
                    revise(c, w, &d, &d.title, &d.description, &body, b.message.as_deref(), DEv::new(DocEventKind::Revised), &b.paths, now).await?;
                charge(c, w.org_id, bytes, w.quota).await?;
                let after = must(c, w.org_id, w.num).await?;
                let mut ch = changed(d, after);
                ch.revised = revised;
                ch.unchanged = !revised;
                if revised {
                    ch.fields.push("body".into());
                }
                ch.dangling = dangling_of(c, ch.doc.id).await?;
                Ok(ch)
            }

            pub async fn change(c: &mut Conn, w: &DocWrite, ch: &DocChange) -> Result<DocChanged> {
                lock(c, w.org_id).await?;
                let now = now_ms();
                let d = must(c, w.org_id, w.num).await?;
                check_rev(w, &d)?;
                let revise_fields = ch.title.is_some()
                    || ch.description.is_some()
                    || ch.slug.is_some()
                    || ch.labels.is_some()
                    || ch.team_id.is_some()
                    || ch.parent.is_some()
                    || ch.supplements.is_some()
                    || ch.related.is_some();
                if revise_fields {
                    can_revise(c, w, &d).await?;
                }
                if ch.owner_id.is_some() {
                    can_manage(w, &d, "owner")?;
                }
                if ch.editors.is_some() {
                    can_manage(w, &d, "editors")?;
                }
                if ch.status.is_some() {
                    can_manage(w, &d, "status")?;
                }
                if ch.pinned.is_some() {
                    can_manage(w, &d, "pinning")?;
                }
                if ch.expires_at.is_some() {
                    can_manage(w, &d, "expiry")?;
                }
                let wb = w.written_by.as_deref();
                let mut bytes = 0i64;
                let mut fields: Vec<String> = Vec::new();
                let mut related_to = Vec::new();
                // Title and description: a revision with the same body.
                let title = ch.title.clone().unwrap_or(d.title.clone());
                let description = ch.description.clone().unwrap_or(d.description.clone());
                let mut revised = false;
                if title != d.title || description != d.description {
                    let (b, r) = revise(c, w, &d, &title, &description, &d.body, ch.message.as_deref(), DEv::new(DocEventKind::Revised), &[], now).await?;
                    bytes += b;
                    revised = r;
                    if title != d.title {
                        fields.push("title".into());
                    }
                    if description != d.description {
                        fields.push("description".into());
                    }
                }
                let mut status = d.status;
                if let Some(s) = ch.status
                    && s != d.status
                {
                    use DocStatus::*;
                    let ok = matches!(
                        (d.status, s),
                        (Draft, Active) | (Draft, Archived) | (Active, Archived) | (Archived, Draft) | (Superseded, Archived)
                    );
                    if s == Superseded {
                        return Err(refused(
                            "invalid_transition",
                            format!("a document becomes superseded when another replaces it: amail doc supersede d{} --by dN", d.num),
                        ));
                    }
                    if !ok {
                        return Err(refused(
                            "invalid_transition",
                            format!("d{} is {}: it cannot become {s}", d.num, d.status),
                        ));
                    }
                    if s == Active {
                        let k = must_kind(c, w.org_id, &d.kind).await?;
                        let human = k.activate_by == ActivateBy::Human || team_human(c, d.team_id).await?;
                        if human && !w.is_user {
                            return Err(refused(
                                "not_owner",
                                if k.activate_by == ActivateBy::Human {
                                    format!("documents of kind {} are activated by the user; ask the user to review d{}", k.name, d.num)
                                } else {
                                    format!("the team of d{} has its documents activated by the user; ask the user to review it", d.num)
                                },
                            ));
                        }
                    }
                    status = s;
                }
                let slug = match &ch.slug {
                    Some(s) => {
                        if let Some(s) = s {
                            check_slug(c, w.org_id, s, Some(d.num)).await?;
                        }
                        s.clone()
                    }
                    None => d.slug.clone(),
                };
                let labels = ch.labels.clone().unwrap_or(d.labels.clone());
                let editors = ch.editors.clone().unwrap_or(d.editors.clone());
                let team_id = ch.team_id.unwrap_or(d.team_id);
                let owner_id = ch.owner_id.unwrap_or(d.owner_id);
                let pinned = ch.pinned.unwrap_or(d.pinned);
                let expires_at = ch.expires_at.unwrap_or(d.expires_at);
                sqlx::query(
                    "UPDATE docs SET slug = $2, labels = $3, editors = $4, team_id = $5, owner_id = $6, pinned = $7, \
                     expires_at = $8, status = $9, \
                     expiry_reminded_at = CASE WHEN $10 THEN NULL ELSE expiry_reminded_at END WHERE id = $1",
                )
                .bind(d.id)
                .bind(&slug)
                .bind(labels_to_db(&labels))
                .bind(labels_to_db(&editors))
                .bind(team_id)
                .bind(owner_id)
                .bind(pinned)
                .bind(expires_at)
                .bind(status.as_str())
                .bind(expires_at != d.expires_at)
                .execute(&mut *c)
                .await?;
                let after_fields = must(c, w.org_id, w.num).await?;
                let mut evs: Vec<DEv> = Vec::new();
                let mut ch_ev = |name: &str, from: Option<String>, to: Option<String>| {
                    if from != to {
                        fields.push(name.to_string());
                        evs.push(DEv::new(DocEventKind::Changed).field(name).from(from).to(to));
                    }
                };
                let list = |v: &[String]| (!v.is_empty()).then(|| v.join(","));
                let ts = |v: Option<i64>| v.map($crate::tasks::ts);
                ch_ev("status", Some(d.status.to_string()), Some(after_fields.status.to_string()));
                ch_ev("slug", d.slug.clone(), after_fields.slug.clone());
                ch_ev("labels", list(&d.labels), list(&after_fields.labels));
                ch_ev("editors", list(&d.editors), list(&after_fields.editors));
                ch_ev("team", d.team.clone(), after_fields.team.clone());
                ch_ev("owner", Some(d.owner.clone()), Some(after_fields.owner.clone()));
                ch_ev("pinned", Some(d.pinned.to_string()), Some(after_fields.pinned.to_string()));
                ch_ev("expires_at", ts(d.expires_at), ts(after_fields.expires_at));
                for e in evs {
                    bytes += event(c, d.id, w.org_id, w.actor_id, wb, now, e).await?;
                }
                // The row's fields (title and description were counted with
                // the revision).
                bytes += doc_row_bytes(&after_fields)
                    - doc_row_bytes(&d)
                    - (after_fields.title.len() as i64 - d.title.len() as i64)
                    - (after_fields.description.len() as i64 - d.description.len() as i64);
                // Relations.
                if let Some(p) = ch.parent {
                    let target = match p {
                        Some(pn) => {
                            let p = must(c, w.org_id, pn).await?;
                            if d.parent != Some(pn) {
                                check_parent(c, Some(d.id), &p).await?;
                            }
                            Some(p)
                        }
                        None => None,
                    };
                    if target.as_ref().map(|t| t.num) != d.parent {
                        bytes += set_relation(c, w.org_id, d.id, "parent", target.as_ref().map(|t| t.id), now).await?;
                        let e = DEv::new(DocEventKind::Relation).field("parent").from(d.parent.map(dnum)).to(target.as_ref().map(|t| dnum(t.num)));
                        bytes += event(c, d.id, w.org_id, w.actor_id, wb, now, e).await?;
                        fields.push("parent".into());
                    }
                }
                if let Some(s) = ch.supplements {
                    let target = match s {
                        Some(sn) => {
                            if sn == d.num {
                                return Err(refused("invalid_relation", "a document cannot supplement itself"));
                            }
                            Some(must(c, w.org_id, sn).await?)
                        }
                        None => None,
                    };
                    if target.as_ref().map(|t| t.num) != d.supplements {
                        bytes += set_relation(c, w.org_id, d.id, "supplements", target.as_ref().map(|t| t.id), now).await?;
                        let e = DEv::new(DocEventKind::Relation).field("supplements").from(d.supplements.map(dnum)).to(target.as_ref().map(|t| dnum(t.num)));
                        bytes += event(c, d.id, w.org_id, w.actor_id, wb, now, e).await?;
                        if let Some(t) = target {
                            let e = DEv::new(DocEventKind::Relation).field("supplemented_by").to(Some(dnum(d.num)));
                            bytes += event(c, t.id, w.org_id, w.actor_id, wb, now, e).await?;
                            related_to.push(("supplements".to_string(), t));
                        }
                        fields.push("supplements".into());
                    }
                }
                if let Some(rel) = &ch.related {
                    let mut want: Vec<i64> = rel.iter().copied().filter(|n| *n != d.num).collect();
                    want.sort_unstable();
                    want.dedup();
                    if want != d.related {
                        let n = sqlx::query("DELETE FROM doc_relations WHERE kind = 'related' AND (src_id = $1 OR dst_id = $1)")
                            .bind(d.id)
                            .execute(&mut *c)
                            .await?
                            .rows_affected() as i64;
                        bytes -= n * STORED_BYTES_PER_DOC_ROW;
                        for num in &want {
                            let t = must(c, w.org_id, *num).await?;
                            add_relation(c, w.org_id, d.id, "related", t.id, now).await?;
                            bytes += STORED_BYTES_PER_DOC_ROW;
                        }
                        let e = DEv::new(DocEventKind::Relation)
                            .field("related")
                            .from((!d.related.is_empty()).then(|| d.related.iter().map(|n| dnum(*n)).collect::<Vec<_>>().join(",")))
                            .to((!want.is_empty()).then(|| want.iter().map(|n| dnum(*n)).collect::<Vec<_>>().join(",")));
                        bytes += event(c, d.id, w.org_id, w.actor_id, wb, now, e).await?;
                        fields.push("related".into());
                    }
                }
                if !fields.is_empty() {
                    sqlx::query("UPDATE docs SET updated_at = $2 WHERE id = $1").bind(d.id).bind(now).execute(&mut *c).await?;
                }
                charge(c, w.org_id, bytes, w.quota).await?;
                let after = must(c, w.org_id, w.num).await?;
                let mut out = changed(d, after);
                out.fields = fields;
                out.revised = revised;
                out.related_to = related_to;
                out.dangling = dangling_of(c, out.doc.id).await?;
                Ok(out)
            }

            /// `by` replaces document `w.num`.
            pub async fn supersede(c: &mut Conn, w: &DocWrite, by: i64) -> Result<DocChanged> {
                lock(c, w.org_id).await?;
                let now = now_ms();
                let d = must(c, w.org_id, w.num).await?;
                can_manage(w, &d, "status")?;
                if by == d.num {
                    return Err(refused("invalid_relation", "a document cannot supersede itself"));
                }
                let n = must(c, w.org_id, by).await?;
                if d.status == DocStatus::Superseded {
                    return Err(refused(
                        "invalid_transition",
                        format!("d{} is already superseded by d{}", d.num, d.superseded_by.unwrap_or_default()),
                    ));
                }
                let wb = w.written_by.as_deref();
                sqlx::query("UPDATE docs SET status = 'superseded', updated_at = $2 WHERE id = $1")
                    .bind(d.id)
                    .bind(now)
                    .execute(&mut *c)
                    .await?;
                add_relation(c, w.org_id, n.id, "supersedes", d.id, now).await?;
                let mut bytes = STORED_BYTES_PER_DOC_ROW;
                let e = DEv::new(DocEventKind::Changed).field("status").from(Some(d.status.to_string())).to(Some("superseded".into()));
                bytes += event(c, d.id, w.org_id, w.actor_id, wb, now, e).await?;
                let e = DEv::new(DocEventKind::Relation).field("superseded_by").to(Some(dnum(n.num)));
                bytes += event(c, d.id, w.org_id, w.actor_id, wb, now, e).await?;
                let e = DEv::new(DocEventKind::Relation).field("supersedes").to(Some(dnum(d.num)));
                bytes += event(c, n.id, w.org_id, w.actor_id, wb, now, e).await?;
                charge(c, w.org_id, bytes, w.quota).await?;
                let after = must(c, w.org_id, w.num).await?;
                let mut out = changed(d, after);
                out.fields = vec!["status".into(), "superseded_by".into()];
                out.related_to = vec![("supersedes".to_string(), n)];
                Ok(out)
            }

            /// Confirm the content: `valid_ms` how long (`None`: the kind's
            /// interval; `Some(None)`: no expiry).
            pub async fn verify(c: &mut Conn, w: &DocWrite, valid_ms: Option<Option<i64>>) -> Result<DocChanged> {
                lock(c, w.org_id).await?;
                let now = now_ms();
                let d = must(c, w.org_id, w.num).await?;
                can_manage(w, &d, "verification")?;
                let k = must_kind(c, w.org_id, &d.kind).await?;
                let valid = match valid_ms {
                    Some(v) => v,
                    None => k.verify_days.map(|n| n * 86_400_000),
                };
                let expires = valid.map(|v| now + v);
                let by = seat_name(c, w.actor_id).await?;
                sqlx::query(
                    "UPDATE docs SET verified_at = $2, verified_by = $3, expires_at = $4, expiry_reminded_at = NULL, \
                     updated_at = $2 WHERE id = $1",
                )
                .bind(d.id)
                .bind(now)
                .bind(&by)
                .bind(expires)
                .execute(&mut *c)
                .await?;
                let e = DEv::new(DocEventKind::Verified).to(expires.map($crate::tasks::ts));
                let mut bytes = event(c, d.id, w.org_id, w.actor_id, w.written_by.as_deref(), now, e).await?;
                bytes += by.as_deref().map_or(0, str::len) as i64 - d.verified_by.as_deref().map_or(0, str::len) as i64;
                charge(c, w.org_id, bytes, w.quota).await?;
                let after = must(c, w.org_id, w.num).await?;
                let mut out = changed(d, after);
                out.fields = vec!["verified_at".into(), "expires_at".into()];
                Ok(out)
            }

            /// A new document made by `split` or `extract` (status `draft`
            /// unless `status` is given), with its first revision.
            #[allow(clippy::too_many_arguments)]
            async fn new_part(
                c: &mut Conn,
                w: &DocWrite,
                src: &DocRec,
                kind: &str,
                title: &str,
                body: &str,
                status: DocStatus,
                note: String,
                now: i64,
            ) -> Result<(DocRec, i64)> {
                let num: i64 = sqlx::query_scalar(
                    "UPDATE orgs SET org_doc_seq = org_doc_seq + 1 WHERE id = $1 AND deleted_at IS NULL RETURNING org_doc_seq",
                )
                .bind(w.org_id)
                .fetch_one(&mut *c)
                .await?;
                if let Some(max) = w.bounds.docs_per_org
                    && num > max
                {
                    return Err(refused("too_many_docs", format!("an org has at most {max} documents on this server")));
                }
                sqlx::query(
                    "INSERT INTO docs (org_id, num, kind, title, description, status, owner_id, team_id, labels, created_at, updated_at) \
                     VALUES ($1, $2, $3, $4, '', $5, $6, $7, $8, $9, $9)",
                )
                .bind(w.org_id)
                .bind(num)
                .bind(kind)
                .bind(title)
                .bind(status.as_str())
                .bind(src.owner_id)
                .bind(src.team_id)
                .bind(labels_to_db(&src.labels))
                .bind(now)
                .execute(&mut *c)
                .await?;
                let blank = must(c, w.org_id, num).await?;
                let mut bytes = doc_row_bytes(&blank);
                let ev = DEv::new(DocEventKind::Created).to(Some(title.to_string())).note(Some(note.clone()));
                bytes += revise(c, w, &blank, title, "", body, Some(&note), ev, &[], now).await?.0;
                Ok((must(c, w.org_id, num).await?, bytes))
            }

            /// Rewrite the references to document `num` in other documents
            /// and in task bodies with `map` (anchor or none -> new text);
            /// returns the documents and tasks changed and the bytes.
            #[allow(clippy::too_many_arguments)]
            async fn rewrite_refs(
                c: &mut Conn,
                w: &DocWrite,
                num: i64,
                anchors: Option<&HashSet<String>>,
                map: &(dyn Fn(i64, Option<&str>) -> Option<String> + Sync),
                skip: &[i64],
                note: &str,
                now: i64,
            ) -> Result<(Vec<i64>, Vec<i64>, i64)> {
                let mut bytes = 0;
                let srcs: Vec<(i64, Option<String>)> = sqlx::query_as(
                    "SELECT DISTINCT l.src_doc, l.target_anchor FROM doc_links l WHERE l.org_id = $1 AND l.target_kind = 'doc' \
                     AND l.target_num = $2",
                )
                .bind(w.org_id)
                .bind(num)
                .fetch_all(&mut *c)
                .await?;
                let mut ids: Vec<i64> = srcs
                    .iter()
                    .filter(|(_, a)| match anchors {
                        Some(set) => a.as_ref().is_some_and(|a| set.contains(a)),
                        None => true,
                    })
                    .map(|(id, _)| *id)
                    .collect();
                ids.sort_unstable();
                ids.dedup();
                let mut docs_out = Vec::new();
                for id in ids {
                    let n: i64 = sqlx::query_scalar("SELECT num FROM docs WHERE id = $1").bind(id).fetch_one(&mut *c).await?;
                    if skip.contains(&n) {
                        continue;
                    }
                    let d = must(c, w.org_id, n).await?;
                    let (body, ch) = docmd::rewrite_doc_refs(&d.body, map);
                    if !ch {
                        continue;
                    }
                    let ev = DEv::new(DocEventKind::LinksRewritten).note(Some(note.to_string()));
                    bytes += revise(c, w, &d, &d.title, &d.description, &body, Some(note), ev, &[], now).await?.0;
                    docs_out.push(n);
                }
                // Task bodies.
                let pat = like_pattern(&format!("d{num}"));
                let tasks: Vec<(i64, i64, String)> = sqlx::query_as(
                    "SELECT id, num, body FROM tasks WHERE org_id = $1 AND body LIKE $2 ESCAPE '\\'",
                )
                .bind(w.org_id)
                .bind(pat)
                .fetch_all(&mut *c)
                .await?;
                let mut tasks_out = Vec::new();
                let mut task_bytes = 0;
                for (id, tnum, body) in tasks {
                    let (nb, ch) = docmd::rewrite_doc_refs(&body, map);
                    if !ch {
                        continue;
                    }
                    sqlx::query("UPDATE tasks SET body = $2, updated_at = $3 WHERE id = $1")
                        .bind(id)
                        .bind(&nb)
                        .bind(now)
                        .execute(&mut *c)
                        .await?;
                    let mut e = $crate::tasks::Ev::new(amail_proto::TaskEventKind::Changed)
                        .field("body")
                        .from(Some(body.clone()))
                        .to(Some(nb.clone()));
                    e.note = Some(note.to_string());
                    task_bytes += nb.len() as i64 - body.len() as i64;
                    task_bytes += super::tq::event(c, id, w.org_id, w.actor_id, w.written_by.as_deref(), now, e).await?;
                    tasks_out.push(tnum);
                }
                super::tq::charge(c, w.org_id, task_bytes, w.quota).await?;
                Ok((docs_out, tasks_out, bytes))
            }

            /// Each heading of `level` becomes a child document.
            pub async fn split(c: &mut Conn, w: &DocWrite, level: i64) -> Result<DocMovedRec> {
                lock(c, w.org_id).await?;
                let now = now_ms();
                let d = must(c, w.org_id, w.num).await?;
                check_rev(w, &d)?;
                can_revise(c, w, &d).await?;
                check_frozen(&d)?;
                let p = docmd::parse(&d.body);
                let parts: Vec<docmd::Section> = p.sections.iter().filter(|s| s.level == level).cloned().collect();
                if parts.is_empty() {
                    return Err(refused("nothing_to_split", format!("d{} has no heading of level {level}", d.num)));
                }
                if ancestors(c, d.id).await?.len() + 1 > MAX_DOC_PARENT_DEPTH {
                    return Err(refused(
                        "parent_too_deep",
                        format!("documents nest at most {MAX_DOC_PARENT_DEPTH} levels below a top document"),
                    ));
                }
                let status = if d.status == DocStatus::Active { DocStatus::Active } else { DocStatus::Draft };
                let mut bytes = 0;
                let mut created = Vec::new();
                let mut map: HashMap<String, (i64, Option<String>)> = HashMap::new();
                let mut moved = Vec::new();
                for s in &parts {
                    let body = section_content(&d.body, s);
                    let title: String = if s.heading.is_empty() { format!("Part of d{}", d.num) } else { s.heading.clone() };
                    let title: String = title.chars().take(MAX_DOC_TITLE_CHARS).collect();
                    let (child, b) = new_part(c, w, &d, &d.kind, &title, &body, status, format!("split from d{}#{}", d.num, s.anchor), now).await?;
                    bytes += b;
                    bytes += set_relation(c, w.org_id, child.id, "parent", Some(d.id), now).await?;
                    let e = DEv::new(DocEventKind::Relation).field("parent").to(Some(dnum(d.num)));
                    bytes += event(c, child.id, w.org_id, w.actor_id, w.written_by.as_deref(), now, e).await?;
                    map.insert(s.anchor.clone(), (child.num, None));
                    for (a, b) in nested_map(&p, s, &docmd::parse(&body)) {
                        map.insert(a, (child.num, Some(b)));
                    }
                    moved.push((format!("#{}", s.anchor), dnum(child.num)));
                    created.push(child);
                }
                // The parent keeps the rest, and gets a list of the parts.
                let mut rest = String::new();
                let mut last = 0;
                for s in &parts {
                    rest.push_str(&d.body[last..s.start]);
                    last = s.end;
                }
                rest.push_str(&d.body[last..]);
                let mut body = rest.trim_end().to_string();
                if !body.is_empty() {
                    body.push_str("\n\n");
                }
                for ch in &created {
                    body.push_str(&format!("- d{} {}\n", ch.num, ch.title));
                }
                // Links inside the parent's own text to moved sections.
                let map_ref = &map;
                let num = d.num;
                let f = move |n: i64, a: Option<&str>| -> Option<String> {
                    if n != num {
                        return None;
                    }
                    let (to, sub) = map_ref.get(a?)?;
                    Some(match sub {
                        Some(s) => format!("d{to}#{s}"),
                        None => dnum(*to),
                    })
                };
                let (body, _) = docmd::rewrite_doc_refs(&body, &f);
                let note = format!(
                    "split into {}",
                    created.iter().map(|x| dnum(x.num)).collect::<Vec<_>>().join(", ")
                );
                let ev = DEv::new(DocEventKind::Split).note(Some(note.clone()));
                bytes += revise(c, w, &d, &d.title, &d.description, &body, Some(&note), ev, &[], now).await?.0;
                let set: HashSet<String> = map.keys().cloned().collect();
                let (mut rewritten, tasks_rewritten, b) =
                    rewrite_refs(c, w, d.num, Some(&set), &f, &[d.num], &format!("links to sections of d{} that moved ({note})", d.num), now).await?;
                rewritten.retain(|n| !created.iter().any(|x| x.num == *n));
                bytes += b;
                charge(c, w.org_id, bytes, w.quota).await?;
                Ok(DocMovedRec {
                    doc: Some(must(c, w.org_id, w.num).await?),
                    created: by_ids(c, &created.iter().map(|x| x.id).collect::<Vec<_>>()).await?,
                    rewritten,
                    tasks_rewritten,
                    moved,
                })
            }

            /// Move section `anchor` into a new document that supplements
            /// this one.
            pub async fn extract(c: &mut Conn, w: &DocWrite, anchor: &str, title: &str, kind: Option<&str>) -> Result<DocMovedRec> {
                lock(c, w.org_id).await?;
                let now = now_ms();
                let d = must(c, w.org_id, w.num).await?;
                check_rev(w, &d)?;
                can_revise(c, w, &d).await?;
                check_frozen(&d)?;
                let p = docmd::parse(&d.body);
                let s = p
                    .section(anchor)
                    .filter(|s| s.level > 0)
                    .cloned()
                    .ok_or_else(|| refused("no_section", format!("d{} has no section #{anchor} (amail doc read d{} --sections)", d.num, d.num)))?;
                let k = must_kind(c, w.org_id, kind.unwrap_or(&d.kind)).await?;
                if k.disabled {
                    return Err(refused("kind_disabled", format!("the kind {} takes no new documents", k.name)));
                }
                let body = section_content(&d.body, &s);
                let (part, mut bytes) =
                    new_part(c, w, &d, &k.name, title, &body, DocStatus::Draft, format!("extracted from d{}#{}", d.num, s.anchor), now).await?;
                bytes += set_relation(c, w.org_id, part.id, "supplements", Some(d.id), now).await?;
                let wb = w.written_by.as_deref();
                let e = DEv::new(DocEventKind::Relation).field("supplements").to(Some(dnum(d.num)));
                bytes += event(c, part.id, w.org_id, w.actor_id, wb, now, e).await?;
                let mut map: HashMap<String, (i64, Option<String>)> = HashMap::new();
                map.insert(s.anchor.clone(), (part.num, None));
                for (a, b) in nested_map(&p, &s, &docmd::parse(&body)) {
                    map.insert(a, (part.num, Some(b)));
                }
                let mut new_body = String::new();
                new_body.push_str(&d.body[..s.heading_end]);
                new_body.push_str(&format!("\n\nSee d{}.\n\n", part.num));
                new_body.push_str(&d.body[s.end..]);
                let mut new_body = new_body.trim_end().to_string();
                new_body.push('\n');
                let map_ref = &map;
                let num = d.num;
                let f = move |n: i64, a: Option<&str>| -> Option<String> {
                    if n != num {
                        return None;
                    }
                    let (to, sub) = map_ref.get(a?)?;
                    Some(match sub {
                        Some(s) => format!("d{to}#{s}"),
                        None => dnum(*to),
                    })
                };
                let (new_body, _) = docmd::rewrite_doc_refs(&new_body, &f);
                let note = format!("#{} extracted into d{}", s.anchor, part.num);
                let ev = DEv::new(DocEventKind::Extracted).note(Some(note.clone()));
                bytes += revise(c, w, &d, &d.title, &d.description, &new_body, Some(&note), ev, &[], now).await?.0;
                let set: HashSet<String> = map.keys().cloned().collect();
                let (rewritten, tasks_rewritten, b) = rewrite_refs(c, w, d.num, Some(&set), &f, &[d.num], &note, now).await?;
                bytes += b;
                charge(c, w.org_id, bytes, w.quota).await?;
                Ok(DocMovedRec {
                    doc: Some(must(c, w.org_id, w.num).await?),
                    created: vec![must(c, w.org_id, part.num).await?],
                    rewritten,
                    tasks_rewritten,
                    moved: vec![(format!("#{}", s.anchor), dnum(part.num))],
                })
            }

            /// Append document `w.num` to `into` as a section; the merged
            /// document becomes superseded by `into`.
            pub async fn merge(c: &mut Conn, w: &DocWrite, into: i64) -> Result<DocMovedRec> {
                lock(c, w.org_id).await?;
                let now = now_ms();
                let m = must(c, w.org_id, w.num).await?;
                check_rev(w, &m)?;
                if into == m.num {
                    return Err(refused("invalid_relation", "a document cannot be merged into itself"));
                }
                let t = must(c, w.org_id, into).await?;
                let owner_of_both = w.is_user || (w.actor_id == Some(m.owner_id) && w.actor_id == Some(t.owner_id));
                if !owner_of_both {
                    return Err(refused(
                        "not_owner",
                        format!("merging needs the owner of both documents (d{}: {}, d{}: {}) or the user", m.num, m.owner, t.num, t.owner),
                    ));
                }
                check_frozen(&t)?;
                if m.status == DocStatus::Superseded {
                    return Err(refused("invalid_transition", format!("d{} is already superseded", m.num)));
                }
                let pt = docmd::parse(&t.body);
                let heads: Vec<&docmd::Section> = pt.sections.iter().filter(|s| s.level > 0).collect();
                // The new section's level: the top level of the target, one
                // below a single title heading at its start.
                let mut lv = heads.iter().map(|s| s.level).min().unwrap_or(1);
                if heads.iter().filter(|s| s.level == lv).count() == 1 && heads.first().is_some_and(|s| s.level == lv) {
                    lv += 1;
                }
                let lv = lv.min(6);
                let pm = docmd::parse(&m.body);
                let min_m = pm.sections.iter().filter(|s| s.level > 0).map(|s| s.level).min();
                let shift = match min_m {
                    Some(x) if x <= lv => (lv + 1 - x) as usize,
                    _ => 0,
                };
                let appended = docmd::shift_headings(&m.body, shift);
                let mut body = t.body.trim_end().to_string();
                if !body.is_empty() {
                    body.push_str("\n\n");
                }
                let at = body.len();
                body.push_str(&format!("{} {}\n\n{}", "#".repeat(lv as usize), m.title, appended.trim_matches('\n')));
                body.push('\n');
                let k = must_kind(c, w.org_id, &t.kind).await?;
                check_size(&body, &k)?;
                let pn = docmd::parse(&body);
                let sec = pn
                    .sections
                    .iter()
                    .find(|s| s.start >= at && s.level == lv)
                    .cloned()
                    .ok_or_else(|| StoreError::Invalid("merged section not found".into()))?;
                let mut map: HashMap<Option<String>, String> = HashMap::new();
                map.insert(None, format!("d{}#{}", t.num, sec.anchor));
                let inner_new: Vec<&docmd::Section> = pn.sections.iter().filter(|x| x.level > 0 && x.start > sec.start && x.end <= sec.end).collect();
                let inner_old: Vec<&docmd::Section> = pm.sections.iter().filter(|x| x.level > 0).collect();
                for (a, b) in inner_old.iter().zip(inner_new.iter()) {
                    map.insert(Some(a.anchor.clone()), format!("d{}#{}", t.num, b.anchor));
                }
                let mut moved = vec![(format!("d{}", m.num), format!("d{}#{}", t.num, sec.anchor))];
                for (a, b) in inner_old.iter().zip(inner_new.iter()) {
                    moved.push((format!("d{}#{}", m.num, a.anchor), format!("d{}#{}", t.num, b.anchor)));
                }
                let map_ref = &map;
                let mnum = m.num;
                let f = move |n: i64, a: Option<&str>| -> Option<String> {
                    if n != mnum {
                        return None;
                    }
                    map_ref
                        .get(&a.map(str::to_string))
                        .or_else(|| map_ref.get(&None))
                        .cloned()
                };
                let (body, _) = docmd::rewrite_doc_refs(&body, &f);
                let note = format!("d{} merged into d{}#{}", m.num, t.num, sec.anchor);
                let ev = DEv::new(DocEventKind::Merged).note(Some(note.clone()));
                let mut bytes = revise(c, w, &t, &t.title, &t.description, &body, Some(&note), ev, &[], now).await?.0;
                // The merged document: superseded by the target.
                sqlx::query("UPDATE docs SET status = 'superseded', updated_at = $2 WHERE id = $1")
                    .bind(m.id)
                    .bind(now)
                    .execute(&mut *c)
                    .await?;
                add_relation(c, w.org_id, t.id, "supersedes", m.id, now).await?;
                bytes += STORED_BYTES_PER_DOC_ROW;
                let wb = w.written_by.as_deref();
                let e = DEv::new(DocEventKind::Merged).to(Some(dnum(t.num))).note(Some(note.clone()));
                bytes += event(c, m.id, w.org_id, w.actor_id, wb, now, e).await?;
                let e = DEv::new(DocEventKind::Changed).field("status").from(Some(m.status.to_string())).to(Some("superseded".into()));
                bytes += event(c, m.id, w.org_id, w.actor_id, wb, now, e).await?;
                let (rewritten, tasks_rewritten, b) = rewrite_refs(c, w, m.num, None, &f, &[t.num, m.num], &note, now).await?;
                bytes += b;
                charge(c, w.org_id, bytes, w.quota).await?;
                Ok(DocMovedRec {
                    doc: Some(must(c, w.org_id, m.num).await?),
                    created: vec![must(c, w.org_id, t.num).await?],
                    rewritten,
                    tasks_rewritten,
                    moved,
                })
            }

            fn status_clause(b: &mut sqlx::QueryBuilder<'_, Db>, statuses: &[DocStatus]) {
                b.push(" AND d.status IN (");
                let mut sep = b.separated(", ");
                for s in statuses {
                    sep.push_bind(s.as_str());
                }
                b.push(")");
            }

            pub async fn list(c: &mut Conn, q: &DocQuery) -> Result<Vec<DocRec>> {
                let mut b = sqlx::QueryBuilder::<Db>::new(DOC_SELECT);
                b.push(" WHERE d.org_id = ");
                b.push_bind(q.org_id);
                let statuses = if q.statuses.is_empty() { vec![DocStatus::Draft, DocStatus::Active] } else { q.statuses.clone() };
                status_clause(&mut b, &statuses);
                if let Some(k) = &q.kind {
                    b.push(" AND d.kind = ");
                    b.push_bind(k.clone());
                }
                if let Some(t) = q.team_id {
                    b.push(" AND d.team_id = ");
                    b.push_bind(t);
                }
                if let Some(o) = q.owner_id {
                    b.push(" AND d.owner_id = ");
                    b.push_bind(o);
                }
                if let Some(l) = &q.label {
                    b.push(" AND d.labels LIKE ");
                    b.push_bind(like_pattern(&format!(",{l},")));
                    b.push(" ESCAPE '\\'");
                }
                if q.stale {
                    b.push(" AND d.expires_at IS NOT NULL AND d.expires_at <= ");
                    b.push_bind(q.now);
                }
                if q.pinned {
                    b.push(" AND d.pinned");
                }
                for t in &q.terms {
                    b.push(format!(" AND (d.title {op} ", op = DIALECT.ilike()));
                    b.push_bind(like_pattern(t));
                    b.push(format!(" ESCAPE '\\' OR COALESCE(d.slug, '') {op} ", op = DIALECT.ilike()));
                    b.push_bind(like_pattern(t));
                    b.push(format!(" ESCAPE '\\' OR d.description {op} ", op = DIALECT.ilike()));
                    b.push_bind(like_pattern(t));
                    b.push(" ESCAPE '\\')");
                }
                if let Some(before) = q.before {
                    b.push(" AND d.num < ");
                    b.push_bind(before);
                }
                b.push(" ORDER BY d.num DESC LIMIT ");
                b.push_bind(q.limit);
                let rows: Vec<DocRow> = b.build_query_as().fetch_all(&mut *c).await?;
                let mut out = rows.into_iter().map(DocRow::into_rec).collect::<Result<Vec<_>>>()?;
                fill(c, &mut out).await?;
                Ok(out)
            }

            pub async fn search(c: &mut Conn, q: &DocSearchQuery) -> Result<DocSearchRes> {
                let long: Vec<&String> = q.terms.iter().filter(|t| t.chars().count() >= 3).collect();
                let short: Vec<&String> = q.terms.iter().filter(|t| t.chars().count() < 3).collect();
                let statuses = if q.statuses.is_empty() { vec![DocStatus::Active] } else { q.statuses.clone() };
                let titles_only = long.is_empty();
                let mut b = sqlx::QueryBuilder::<Db>::new("");
                if titles_only {
                    b.push(DOC_SELECT.replacen(
                        "'' AS body,",
                        "'' AS body, '' AS hit_anchor, '' AS hit_path, d.description AS hit_text,",
                        1,
                    ));
                    b.push(" WHERE d.org_id = ");
                    b.push_bind(q.org_id);
                    for t in &short {
                        b.push(format!(" AND (d.title || ' ' || d.description) {} ", DIALECT.ilike()));
                        b.push_bind(like_pattern(t));
                        b.push(" ESCAPE '\\'");
                    }
                } else {
                    let base = DOC_SELECT.replacen(
                        "'' AS body,",
                        "'' AS body, x.anchor AS hit_anchor, x.path AS hit_path, x.text AS hit_text,",
                        1,
                    );
                    match DIALECT {
                        Dialect::Sqlite => {
                            let from = base.replacen(
                                " FROM docs d ",
                                " FROM doc_sections_fts f JOIN doc_sections x ON x.id = f.rowid JOIN docs d ON d.id = x.doc_id ",
                                1,
                            );
                            b.push(from);
                            b.push(" WHERE doc_sections_fts MATCH ");
                            let m: Vec<String> = long.iter().map(|t| format!("\"{}\"", t.replace('"', "\"\""))).collect();
                            b.push_bind(m.join(" "));
                            b.push(" AND x.org_id = ");
                            b.push_bind(q.org_id);
                            for t in &short {
                                b.push(" AND (x.path || ' ' || x.text) LIKE ");
                                b.push_bind(like_pattern(t));
                                b.push(" ESCAPE '\\'");
                            }
                        }
                        Dialect::Postgres => {
                            let from = base.replacen(" FROM docs d ", " FROM doc_sections x JOIN docs d ON d.id = x.doc_id ", 1);
                            b.push(from);
                            b.push(" WHERE x.org_id = ");
                            b.push_bind(q.org_id);
                            for t in long.iter().chain(short.iter()) {
                                b.push(" AND (x.path || ' ' || x.text) ILIKE ");
                                b.push_bind(like_pattern(t));
                                b.push(" ESCAPE '\\'");
                            }
                        }
                    }
                }
                status_clause(&mut b, &statuses);
                if let Some(k) = &q.kind {
                    b.push(" AND d.kind = ");
                    b.push_bind(k.clone());
                }
                if let Some(t) = q.team_id {
                    b.push(" AND d.team_id = ");
                    b.push_bind(t);
                }
                if let Some(o) = q.owner_id {
                    b.push(" AND d.owner_id = ");
                    b.push_bind(o);
                }
                if let Some(l) = &q.label {
                    b.push(" AND d.labels LIKE ");
                    b.push_bind(like_pattern(&format!(",{l},")));
                    b.push(" ESCAPE '\\'");
                }
                b.push(" ORDER BY CASE WHEN d.expires_at IS NOT NULL AND d.expires_at <= ");
                b.push_bind(q.now);
                b.push(" THEN 1 ELSE 0 END, CASE WHEN d.team_id = ");
                b.push_bind(q.caller_team.unwrap_or(-1));
                b.push(" THEN 0 ELSE 1 END, d.updated_at DESC, d.num DESC");
                if !titles_only {
                    b.push(", x.ord");
                }
                b.push(" LIMIT ");
                b.push_bind(q.limit);
                let rows: Vec<HitRow> = b.build_query_as().fetch_all(&mut *c).await?;
                let mut hits = rows.into_iter().map(HitRow::into_hit).collect::<Result<Vec<_>>>()?;
                let mut docs: Vec<DocRec> = hits.iter().map(|h| h.doc.clone()).collect();
                fill(c, &mut docs).await?;
                for h in hits.iter_mut() {
                    if let Some(d) = docs.iter().find(|d| d.id == h.doc.id) {
                        h.doc = d.clone();
                    }
                }
                Ok(DocSearchRes { hits, titles_only })
            }

            /// The per-turn index: `active` documents of kinds in the index,
            /// of `team` or of no team; pinned first, then the team's, then
            /// the org-wide ones, fresh before stale, newest verified first.
            /// Answers at most `scan` documents and how many match.
            pub async fn index(c: &mut Conn, org_id: i64, team: Option<i64>, now: i64, scan: Option<i64>) -> Result<(Vec<DocRec>, i64)> {
                let team = team.unwrap_or(-1);
                let cond = "WHERE d.org_id = $1 AND d.status = 'active' AND (d.team_id = $2 OR d.team_id IS NULL) \
                            AND EXISTS (SELECT 1 FROM doc_kinds k WHERE k.org_id = d.org_id AND k.name = d.kind AND k.in_index)";
                let total: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM docs d {cond}"))
                    .bind(org_id)
                    .bind(team)
                    .fetch_one(&mut *c)
                    .await?;
                let sql = format!(
                    "{DOC_SELECT} {cond} ORDER BY CASE WHEN d.pinned THEN 0 WHEN d.team_id = $2 THEN 1 ELSE 2 END, \
                     CASE WHEN d.expires_at IS NOT NULL AND d.expires_at <= $3 THEN 1 ELSE 0 END, \
                     COALESCE(d.verified_at, d.updated_at) DESC, d.num DESC LIMIT $4"
                );
                let rows: Vec<DocRow> = sqlx::query_as(&sql)
                    .bind(org_id)
                    .bind(team)
                    .bind(now)
                    .bind(scan.unwrap_or(i64::MAX))
                    .fetch_all(&mut *c)
                    .await?;
                let mut out = rows.into_iter().map(DocRow::into_rec).collect::<Result<Vec<_>>>()?;
                fill(c, &mut out).await?;
                Ok((out, total))
            }

            /// Documents of `kind` (the newest `scan`) whose title,
            /// description or slug is like the given ones, best first.
            #[allow(clippy::too_many_arguments)]
            pub async fn candidates(
                c: &mut Conn,
                org_id: i64,
                kind: &str,
                title: &str,
                description: &str,
                slug: Option<&str>,
                except: i64,
                scan: Option<i64>,
            ) -> Result<Vec<(DocRec, f64)>> {
                let rows: Vec<(i64, String, String, Option<String>)> = sqlx::query_as(
                    "SELECT id, title, description, slug FROM docs WHERE org_id = $1 AND kind = $2 AND num <> $3 \
                     ORDER BY num DESC LIMIT $4",
                )
                .bind(org_id)
                .bind(kind)
                .bind(except)
                .bind(scan.unwrap_or(i64::MAX))
                .fetch_all(&mut *c)
                .await?;
                let slug_words = |s: &str| s.replace(['.', '-', '_'], " ");
                let mut scored: Vec<(i64, f64)> = Vec::new();
                for (id, t, d, s) in rows {
                    let mut score = docmd::similarity(title, &t);
                    if !description.is_empty() && !d.is_empty() {
                        score = score.max(docmd::similarity(description, &d));
                    }
                    if let Some(s) = &s {
                        score = score.max(docmd::similarity(title, &slug_words(s)));
                        if slug.is_some_and(|x| x == s || x.starts_with(&format!("{s}.")) || s.starts_with(&format!("{x}."))) {
                            score = score.max(1.0);
                        }
                    }
                    if score >= DOC_CANDIDATE_SIMILARITY {
                        scored.push((id, score));
                    }
                }
                scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                scored.truncate(DOC_CANDIDATES);
                let docs = by_ids(c, &scored.iter().map(|x| x.0).collect::<Vec<_>>()).await?;
                Ok(scored
                    .into_iter()
                    .filter_map(|(id, s)| docs.iter().find(|d| d.id == id).map(|d| (d.clone(), s)))
                    .collect())
            }

            pub async fn sections(c: &mut Conn, doc_id: i64) -> Result<Vec<DocSectionRec>> {
                let rows: Vec<(String, i64, String, String, i64, i64, i64)> = sqlx::query_as(
                    "SELECT anchor, level, heading, path, start_byte, end_byte, tokens FROM doc_sections WHERE doc_id = $1 ORDER BY ord",
                )
                .bind(doc_id)
                .fetch_all(&mut *c)
                .await?;
                Ok(rows
                    .into_iter()
                    .map(|(anchor, level, heading, path, start, end, tokens)| DocSectionRec { anchor, level, heading, path, start, end, tokens })
                    .collect())
            }

            pub async fn detail(c: &mut Conn, org_id: i64, num: i64, nevents: i64) -> Result<Option<DocDetailRec>> {
                let Some(doc) = by_num(c, org_id, num).await? else { return Ok(None) };
                let sections = sections(c, doc.id).await?;
                let rels: Vec<(i64, String, i64)> = sqlx::query_as(
                    "SELECT src_id, kind, dst_id FROM doc_relations WHERE src_id = $1 OR dst_id = $1",
                )
                .bind(doc.id)
                .fetch_all(&mut *c)
                .await?;
                let ids: Vec<i64> = rels.iter().map(|r| if r.0 == doc.id { r.2 } else { r.0 }).collect();
                let relatives = by_ids(c, &ids).await?;
                let num_of = |id: i64| relatives.iter().find(|d| d.id == id).map(|d| d.num);
                let children: Vec<i64> = rels.iter().filter(|r| r.1 == "parent" && r.2 == doc.id).filter_map(|r| num_of(r.0)).collect();
                let supplemented_by: Vec<i64> =
                    rels.iter().filter(|r| r.1 == "supplements" && r.2 == doc.id).filter_map(|r| num_of(r.0)).collect();
                let (links, linked_from) = links(c, org_id, &doc).await?;
                let threads: Vec<(i64, Option<String>, i64)> = sqlx::query_as(
                    "SELECT l.thread_id, s.name, l.created_at FROM doc_threads l LEFT JOIN seats s ON s.id = l.seat_id \
                     WHERE l.doc_id = $1 ORDER BY l.created_at, l.thread_id",
                )
                .bind(doc.id)
                .fetch_all(&mut *c)
                .await?;
                let task_ids: Vec<i64> = sqlx::query_scalar("SELECT task_id FROM doc_task_links WHERE doc_id = $1 ORDER BY task_id")
                    .bind(doc.id)
                    .fetch_all(&mut *c)
                    .await?;
                let tasks = super::tq::by_ids(c, &task_ids).await?;
                let have: HashSet<&str> = sections.iter().map(|s| s.anchor.as_str()).collect();
                let crows: Vec<(i64, i64, Option<String>, Option<String>, Option<String>)> = sqlx::query_as(
                    "SELECT e.message_id, e.at, s.name, e.note, e.to_value FROM doc_events e LEFT JOIN seats s ON s.id = e.actor_id \
                     WHERE e.doc_id = $1 AND e.kind = 'commented' AND e.message_id IS NOT NULL ORDER BY e.seq DESC LIMIT $2",
                )
                .bind(doc.id)
                .bind(MAX_DOC_PAGE)
                .fetch_all(&mut *c)
                .await?;
                let mut comments: Vec<DocCommentRec> = crows
                    .into_iter()
                    .map(|(message_id, at, by, anchor, path)| DocCommentRec {
                        removed: anchor.as_deref().is_some_and(|a| !have.contains(a)),
                        message_id,
                        at,
                        by,
                        anchor,
                        path,
                    })
                    .collect();
                comments.reverse();
                let sql = format!("{EVENT_SELECT} WHERE e.doc_id = $1 ORDER BY e.seq DESC LIMIT $2");
                let rows: Vec<EventRow> = sqlx::query_as(&sql).bind(doc.id).bind(nevents).fetch_all(&mut *c).await?;
                let mut events = rows.into_iter().map(EventRow::into_rec).collect::<Result<Vec<_>>>()?;
                events.reverse();
                Ok(Some(DocDetailRec {
                    doc,
                    sections,
                    relatives,
                    children,
                    supplemented_by,
                    links,
                    linked_from,
                    threads: threads
                        .into_iter()
                        .map(|(thread_id, seat, created_at)| TaskLinkRec { thread_id, seat, created_at })
                        .collect(),
                    tasks,
                    comments,
                    events,
                }))
            }

            /// Links out of `d` and into it from other documents.
            pub async fn links(c: &mut Conn, org_id: i64, d: &DocRec) -> Result<(Vec<DocLinkRec>, Vec<DocLinkRec>)> {
                let sql = format!("{LINK_SELECT} WHERE l.src_doc = $1 ORDER BY l.id");
                let out: Vec<LinkRow> = sqlx::query_as(&sql).bind(d.id).fetch_all(&mut *c).await?;
                let sql = format!(
                    "{LINK_SELECT} WHERE l.org_id = $1 AND l.target_kind = 'doc' AND l.target_num = $2 AND l.src_doc <> $3 \
                     ORDER BY s.num, l.id LIMIT $4"
                );
                let inn: Vec<LinkRow> =
                    sqlx::query_as(&sql).bind(org_id).bind(d.num).bind(d.id).bind(MAX_DOC_PAGE).fetch_all(&mut *c).await?;
                Ok((out.into_iter().map(LinkRow::into_rec).collect(), inn.into_iter().map(LinkRow::into_rec).collect()))
            }

            /// Dangling and stale links of the org's documents (of `team`).
            pub async fn check(c: &mut Conn, org_id: i64, team: Option<i64>, limit: i64) -> Result<Vec<DocLinkRec>> {
                let sql = format!(
                    "{LINK_SELECT} WHERE l.org_id = $1 AND s.status IN ('draft', 'active') AND ($2 < 0 OR s.team_id = $2) \
                     AND (({DANGLING}) OR (l.target_kind = 'path' AND l.path_exists = FALSE)) ORDER BY s.num, l.id LIMIT $3"
                );
                let rows: Vec<LinkRow> =
                    sqlx::query_as(&sql).bind(org_id).bind(team.unwrap_or(-1)).bind(limit).fetch_all(&mut *c).await?;
                Ok(rows.into_iter().map(LinkRow::into_rec).collect())
            }

            pub async fn revision(c: &mut Conn, org_id: i64, num: i64, rev: i64) -> Result<Option<DocRevRec>> {
                let r: Option<(i64, String, String, String, String, Option<String>, Option<String>, Option<String>, i64)> = sqlx::query_as(
                    "SELECT r.rev, r.title, r.description, r.body, r.sha256, r.message, s.name, r.written_by, r.at \
                     FROM doc_revisions r JOIN docs d ON d.id = r.doc_id LEFT JOIN seats s ON s.id = r.author_id \
                     WHERE d.org_id = $1 AND d.num = $2 AND r.rev = $3",
                )
                .bind(org_id)
                .bind(num)
                .bind(rev)
                .fetch_optional(&mut *c)
                .await?;
                Ok(r.map(|(rev, title, description, body, sha256, message, author, written_by, at)| DocRevRec {
                    rev,
                    title,
                    description,
                    bytes: body.len() as i64,
                    body,
                    sha256,
                    message,
                    author,
                    written_by,
                    at,
                }))
            }

            pub async fn revisions(c: &mut Conn, org_id: i64, num: i64) -> Result<Vec<DocRevRec>> {
                let rows: Vec<(i64, String, String, i64, String, Option<String>, Option<String>, Option<String>, i64)> = sqlx::query_as(
                    "SELECT r.rev, r.title, r.description, CAST(octet_length(r.body) AS BIGINT), r.sha256, r.message, s.name, r.written_by, r.at \
                     FROM doc_revisions r JOIN docs d ON d.id = r.doc_id LEFT JOIN seats s ON s.id = r.author_id \
                     WHERE d.org_id = $1 AND d.num = $2 ORDER BY r.rev",
                )
                .bind(org_id)
                .bind(num)
                .fetch_all(&mut *c)
                .await?;
                Ok(rows
                    .into_iter()
                    .map(|(rev, title, description, bytes, sha256, message, author, written_by, at)| DocRevRec {
                        rev,
                        title,
                        description,
                        body: String::new(),
                        bytes,
                        sha256,
                        message,
                        author,
                        written_by,
                        at,
                    })
                    .collect())
            }

            pub async fn events(c: &mut Conn, org_id: i64, num: i64, after: i64, limit: i64) -> Result<Option<Vec<DocEventRec>>> {
                let id: Option<i64> = sqlx::query_scalar("SELECT id FROM docs WHERE org_id = $1 AND num = $2")
                    .bind(org_id)
                    .bind(num)
                    .fetch_optional(&mut *c)
                    .await?;
                let Some(id) = id else { return Ok(None) };
                let sql = format!("{EVENT_SELECT} WHERE e.doc_id = $1 AND e.seq > $2 ORDER BY e.seq LIMIT $3");
                let rows: Vec<EventRow> = sqlx::query_as(&sql).bind(id).bind(after).bind(limit).fetch_all(&mut *c).await?;
                Ok(Some(rows.into_iter().map(EventRow::into_rec).collect::<Result<Vec<_>>>()?))
            }

            /// The events after the one that wrote revision `rev`.
            pub async fn events_since_rev(c: &mut Conn, org_id: i64, num: i64, rev: i64, limit: i64) -> Result<Vec<DocEventRec>> {
                let seq: Option<i64> = sqlx::query_scalar(
                    "SELECT MAX(e.seq) FROM doc_events e JOIN docs d ON d.id = e.doc_id WHERE d.org_id = $1 AND d.num = $2 \
                     AND e.rev IS NOT NULL AND e.rev <= $3",
                )
                .bind(org_id)
                .bind(num)
                .bind(rev)
                .fetch_one(&mut *c)
                .await?;
                Ok(events(c, org_id, num, seq.unwrap_or(0), limit).await?.unwrap_or_default())
            }

            /// Record a comment; the first one makes `thread_id` the
            /// document's discussion thread.
            pub async fn comment(c: &mut Conn, w: &DocWrite, message_id: i64, thread_id: i64, anchor: Option<&str>, path: Option<&str>) -> Result<DocRec> {
                lock(c, w.org_id).await?;
                let now = now_ms();
                let d = must(c, w.org_id, w.num).await?;
                sqlx::query("UPDATE docs SET thread_id = $2 WHERE id = $1 AND thread_id IS NULL")
                    .bind(d.id)
                    .bind(thread_id)
                    .execute(&mut *c)
                    .await?;
                let mut e = DEv::new(DocEventKind::Commented).note(anchor.map(str::to_string)).to(path.map(str::to_string));
                e.message_id = Some(message_id);
                let bytes = event(c, d.id, w.org_id, w.actor_id, w.written_by.as_deref(), now, e).await?;
                charge(c, w.org_id, bytes, None).await?;
                must(c, w.org_id, w.num).await
            }

            pub async fn commenters(c: &mut Conn, org_id: i64, num: i64) -> Result<Vec<String>> {
                let rows: Vec<(String, i64)> = sqlx::query_as(
                    "SELECT s.name, MIN(e.seq) AS first FROM doc_events e JOIN docs d ON d.id = e.doc_id \
                     JOIN seats s ON s.id = e.actor_id WHERE d.org_id = $1 AND d.num = $2 AND e.kind = 'commented' \
                     GROUP BY s.name ORDER BY first LIMIT 500",
                )
                .bind(org_id)
                .bind(num)
                .fetch_all(&mut *c)
                .await?;
                Ok(rows.into_iter().map(|r| r.0).collect())
            }

            /// Link a thread (a `[d12]` subject); false when the document
            /// does not exist, the thread is its discussion thread or is
            /// linked already.
            pub async fn add_thread(c: &mut Conn, w: &DocWrite, thread_id: i64, note: &str) -> Result<bool> {
                lock(c, w.org_id).await?;
                let now = now_ms();
                let Some(d) = by_num(c, w.org_id, w.num).await? else { return Ok(false) };
                if d.thread_id == Some(thread_id) {
                    return Ok(false);
                }
                let n = sqlx::query(
                    "INSERT INTO doc_threads (doc_id, thread_id, org_id, seat_id, created_at) VALUES ($1, $2, $3, $4, $5) \
                     ON CONFLICT DO NOTHING",
                )
                .bind(d.id)
                .bind(thread_id)
                .bind(w.org_id)
                .bind(w.actor_id)
                .bind(now)
                .execute(&mut *c)
                .await?
                .rows_affected();
                if n == 0 {
                    return Ok(false);
                }
                let e = DEv::new(DocEventKind::Linked).field("thread").to(Some(thread_id.to_string())).note(Some(note.to_string()));
                let bytes = STORED_BYTES_PER_DOC_ROW + event(c, d.id, w.org_id, w.actor_id, w.written_by.as_deref(), now, e).await?;
                charge(c, w.org_id, bytes, None).await?;
                Ok(true)
            }

            /// Link task `task_id` to the documents `nums` its title cites;
            /// returns the numbers newly linked.
            pub async fn link_task(c: &mut Conn, w: &DocWrite, task_id: i64, task_num: i64, nums: &[i64]) -> Result<Vec<i64>> {
                lock(c, w.org_id).await?;
                let now = now_ms();
                let mut out = Vec::new();
                let mut bytes = 0;
                for num in nums {
                    let id: Option<i64> = sqlx::query_scalar("SELECT id FROM docs WHERE org_id = $1 AND num = $2")
                        .bind(w.org_id)
                        .bind(num)
                        .fetch_optional(&mut *c)
                        .await?;
                    let Some(id) = id else { continue };
                    let n = sqlx::query(
                        "INSERT INTO doc_task_links (doc_id, task_id, org_id, created_at) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
                    )
                    .bind(id)
                    .bind(task_id)
                    .bind(w.org_id)
                    .bind(now)
                    .execute(&mut *c)
                    .await?
                    .rows_affected();
                    if n > 0 {
                        let e = DEv::new(DocEventKind::Linked).field("task").to(Some(format!("t{task_num}")));
                        bytes += STORED_BYTES_PER_DOC_ROW + event(c, id, w.org_id, w.actor_id, w.written_by.as_deref(), now, e).await?;
                        out.push(*num);
                    }
                }
                charge(c, w.org_id, bytes, None).await?;
                Ok(out)
            }

            /// Documents whose discussion thread `thread_id` is, or that
            /// it cites.
            pub async fn thread_docs(c: &mut Conn, org_id: i64, thread_id: i64) -> Result<Vec<DocRec>> {
                let ids: Vec<i64> = sqlx::query_scalar(
                    "SELECT id FROM docs WHERE org_id = $1 AND thread_id = $2 \
                     UNION SELECT doc_id FROM doc_threads WHERE org_id = $1 AND thread_id = $2 LIMIT 200",
                )
                .bind(org_id)
                .bind(thread_id)
                .fetch_all(&mut *c)
                .await?;
                by_ids(c, &ids).await
            }

            pub async fn reminder(c: &mut Conn, org_id: i64, doc_id: i64, kind: &str, to: &str, message_id: i64) -> Result<()> {
                lock(c, org_id).await?;
                let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM docs WHERE id = $1 AND org_id = $2")
                    .bind(doc_id)
                    .bind(org_id)
                    .fetch_optional(&mut *c)
                    .await?;
                if exists.is_none() {
                    return Ok(());
                }
                let mut e = DEv::new(DocEventKind::ReminderSent).field(kind).to(Some(to.to_string()));
                e.message_id = Some(message_id);
                let bytes = event(c, doc_id, org_id, None, None, now_ms(), e).await?;
                charge(c, org_id, bytes, None).await
            }

            /// Mark up to `limit` documents whose `expires_at` passed and
            /// that were not reminded as reminded, and return them.
            pub async fn claim_expired(c: &mut Conn, now: i64, limit: i64) -> Result<Vec<DocRec>> {
                let ids: Vec<i64> = sqlx::query_scalar(
                    "UPDATE docs SET expiry_reminded_at = $1 WHERE id IN (SELECT id FROM docs WHERE expires_at IS NOT NULL \
                     AND expiry_reminded_at IS NULL AND expires_at <= $1 AND status IN ('draft', 'active') \
                     ORDER BY expires_at LIMIT $2) AND expiry_reminded_at IS NULL RETURNING id",
                )
                .bind(now)
                .bind(limit)
                .fetch_all(&mut *c)
                .await?;
                by_ids(c, &ids).await
            }

            /// Mark up to `limit` documents with dangling links that were
            /// not reminded of them for a day as reminded, and return them
            /// with those links.
            pub async fn claim_dangling(c: &mut Conn, now: i64, limit: i64) -> Result<Vec<(DocRec, Vec<DocLinkRec>)>> {
                let sql = format!(
                    "UPDATE docs SET dangling_reminded_at = $1 WHERE id IN (SELECT DISTINCT l.src_doc FROM doc_links l \
                     JOIN docs s ON s.id = l.src_doc \
                     LEFT JOIN docs t ON l.target_kind = 'doc' AND t.org_id = l.org_id AND t.num = l.target_num \
                     WHERE {DANGLING} AND s.status IN ('draft', 'active') \
                     AND (s.dangling_reminded_at IS NULL OR s.dangling_reminded_at <= $2) LIMIT $3) \
                     AND (dangling_reminded_at IS NULL OR dangling_reminded_at <= $2) RETURNING id"
                );
                let ids: Vec<i64> = sqlx::query_scalar(&sql)
                    .bind(now)
                    .bind(now - DOC_DANGLING_REMIND_HOURS * 3_600_000)
                    .bind(limit)
                    .fetch_all(&mut *c)
                    .await?;
                let docs = by_ids(c, &ids).await?;
                let mut out = Vec::new();
                for d in docs {
                    let links = dangling_of(c, d.id).await?;
                    out.push((d, links));
                }
                Ok(out)
            }
        }
    };
}
