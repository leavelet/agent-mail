//! Wire types of documents (`/v1/docs…`): an org's knowledge base. Each
//! document has a number (`d12`), an owner, a kind, a status, a one-line
//! description and a Markdown body kept whole per revision.
//!
//! Documents are numbered per org (`num`, shown as `d12`); numbers are never
//! reused. A slug (`runbook.deploy.rollback`) is accepted wherever `dN` is.
//! Everything here is visible to every seat of the owning org and to nothing
//! outside it.

use super::{Deserialize, FromStr, Serialize, TaskRef, double_option, fmt};

string_enum!(
    /// A document's status. A new document is `draft`; `active` documents
    /// of kinds in the index are shown to seats every turn; `superseded`
    /// documents have a replacement (`superseded_by`); `archived` ones are
    /// no longer true and have none.
    DocStatus {
        Draft => "draft",
        Active => "active",
        Superseded => "superseded",
        Archived => "archived",
    }
);

string_enum!(
    /// Who may move a document of a kind from `draft` to `active`.
    ActivateBy { Owner => "owner", Human => "human" }
);

string_enum!(
    /// The kind of a document event (see [`DocEvent`]).
    DocEventKind {
        Created => "created",
        Revised => "revised",
        Changed => "changed",
        Relation => "relation",
        Verified => "verified",
        Split => "split",
        Extracted => "extracted",
        Merged => "merged",
        LinksRewritten => "links_rewritten",
        Linked => "linked",
        Commented => "commented",
        ReminderSent => "reminder_sent",
    }
);

/// A document as answers show it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Doc {
    pub num: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    pub kind: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub status: DocStatus,
    /// The owner seat.
    pub owner: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    /// Seats the owner allows to revise the body, title, description and
    /// labels.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub editors: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
    /// The current revision (1 for a new document).
    pub rev: i64,
    /// Bytes of the body.
    pub bytes: i64,
    /// The token estimate of the body: one per 4 characters of text that is
    /// not CJK, one per CJK character, rounded up.
    pub tokens: i64,
    /// SHA-256 of the body, hex.
    pub sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// `expires_at` has passed: the content should be confirmed again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stale: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<i64>,
    /// The documents this one supersedes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replaces: Vec<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supplements: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<i64>,
    /// The discussion thread (created by the first comment).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<i64>,
    /// The body. Listings leave it out (empty); `GET /docs/{num}` carries
    /// the whole body unless a revision or a section is asked for.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body: String,
    /// One line of advice while the body is large (split it), or an
    /// `index` document is long.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advice: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// A document named from another document: its number, slug, kind, title
/// and status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocRef {
    pub num: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    pub kind: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub status: DocStatus,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stale: bool,
}

/// One section of the current revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocSection {
    /// The anchor (`dN#anchor`); empty for the text before the first
    /// heading.
    pub anchor: String,
    pub level: i64,
    pub heading: String,
    /// The heading path, `Deploy > Rollback`.
    pub path: String,
    /// Byte range of the section in the body, with its subsections.
    pub start: i64,
    pub end: i64,
    /// Token estimate of the section with its subsections.
    pub tokens: i64,
}

/// A link out of a document's body (or, in `linked_from`, into a
/// document).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocLink {
    /// The document the link is in.
    pub src: i64,
    /// The section of `src` the link is in (empty: before the first
    /// heading).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub src_anchor: String,
    /// `doc`, `task`, `message`, `thread`, `seat` or `path`.
    pub kind: String,
    /// The target as written: `d12#rollback`, `t3`, `#45`, `thread 7`,
    /// `@w1`, `src/main.rs:10`.
    pub target: String,
    /// The number of a document, task, message or thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub num: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
    /// The target's title when the link was written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_at_write: Option<String>,
    /// The target's title now (documents and tasks).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The target was renamed since the link was written.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub renamed: bool,
    /// A document that does not exist, or an anchor it no longer has.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dangling: bool,
    /// A repository path the node reported missing.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stale: bool,
}

/// One entry of a document's history. Events are appended and never
/// changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocEvent {
    pub seq: i64,
    pub at: String,
    /// The seat that made the change; absent for what the server did on its
    /// own (reminders).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub written_by: Option<String>,
    pub kind: DocEventKind,
    /// `changed`: the field (`status`, `owner`, `slug`, `labels`, `team`,
    /// `editors`, `pinned`, `expires_at`). `relation`: `parent`,
    /// `supersedes`, `superseded_by`, `supplements`, `supplemented_by`,
    /// `related`. `reminder_sent`: the reminder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// `revised`: the revision message. `commented`: the anchor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<i64>,
    /// The revision this event created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<i64>,
}

/// One revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocRevision {
    pub rev: i64,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub written_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub sha256: String,
    pub bytes: i64,
    pub at: String,
}

/// A comment of a document (a message in its discussion thread).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocComment {
    pub message_id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    pub at: String,
    /// The section the comment is on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
    /// The section's heading path when the comment was written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The current revision no longer has the section: shown as `on a
    /// removed section (was: <path>)`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub removed: bool,
}

/// A thread linked to a document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocThreadLink {
    pub thread_id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linked_by: Option<String>,
    pub linked_at: String,
}

/// An existing document like a new one (see [`DocDetail::candidates`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocCandidate {
    pub doc: DocRef,
    /// Trigram similarity of the titles, descriptions or slugs (0 to 1).
    pub similarity: f64,
}

/// Where a section went (`split`, `extract`, `merge`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocMove {
    /// `#rollback` of the source document.
    pub from: String,
    /// `d31` or `d12#rollback`.
    pub to: String,
}

/// A part of a document read on its own: a section or an old revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocText {
    pub rev: i64,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub text: String,
    pub tokens: i64,
}

/// Answer of `GET /v1/docs/{num}` and of every write of one document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocDetail {
    pub doc: Doc,
    /// With `?rev=` or `?section=`: what was asked for (`doc.body` is then
    /// left out).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read: Option<DocText>,
    /// The sections of the current revision.
    #[serde(default)]
    pub sections: Vec<DocSection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<DocRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<DocRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<DocRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replaces: Vec<DocRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supplements: Option<DocRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supplemented_by: Vec<DocRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related: Vec<DocRef>,
    /// Links out of the current revision.
    #[serde(default)]
    pub links: Vec<DocLink>,
    /// Links into this document from other documents (`Linked from:`).
    #[serde(default)]
    pub linked_from: Vec<DocLink>,
    /// Threads citing it (`[d12]` subjects) and tasks whose title cites it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub threads: Vec<DocThreadLink>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<TaskRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comments: Vec<DocComment>,
    /// The newest events (at most [`crate::limits::DOC_SHOWN_EVENTS`]),
    /// oldest first.
    #[serde(default)]
    pub events: Vec<DocEvent>,
    #[serde(default)]
    pub event_count: i64,
    /// `POST /docs` with a `client_key` used before: the document that key
    /// created; nothing was created.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub existing: bool,
    /// A write whose body, title and description equal the current ones:
    /// no revision was created.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unchanged: bool,
    /// `POST /docs`: existing documents of the same kind with a close
    /// title, description or slug (the create is never refused for them).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<DocCandidate>,
    /// The line shown with `candidates`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidates_line: Option<String>,
    /// After a write: the links of the written revision that are dangling.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dangling: Vec<DocLink>,
    /// `split`, `extract`, `merge`: where the sections went.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub moved: Vec<DocMove>,
}

/// Body of `POST /v1/docs`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NewDoc {
    pub title: String,
    /// A kind of the org (default `reference`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    /// A team of the org (default: the owner's team).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    /// `dN` or a slug.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// `dN` or a slug.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supplements: Option<String>,
    /// The owner seat (default: the caller's seat).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub editors: Vec<String>,
    /// The revision message (at most 500 characters).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// 1 to 64 characters, as for tasks: a second create with the same key
    /// in the same org within 24 hours answers the document the first one
    /// created (200, `existing`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_key: Option<String>,
    /// Record the events but send no notification mail.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub silent: bool,
    /// What the node found for repository paths in the body.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<DocPathCheck>,
}

/// Whether a repository path exists in the working directory of the seat
/// that writes (reported by the node).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DocPathCheck {
    pub path: String,
    pub exists: bool,
}

/// Body of `PUT /v1/docs/{num}/body`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DocBodyWrite {
    /// The new body, or with `section` the new text of that section: the
    /// section's heading stays unless the text starts with a heading.
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Refuse the write with 409 `stale` unless the document's `rev` is
    /// this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_rev: Option<i64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub silent: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<DocPathCheck>,
}

/// Body of `PATCH /v1/docs/{num}`: a missing field is unchanged; `null`
/// clears a field that can be empty.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DocPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub slug: Option<Option<String>>,
    /// Replaces the labels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<String>>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub team: Option<Option<String>>,
    /// `dN` or a slug.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub parent: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub supplements: Option<Option<String>>,
    /// Replaces the `related` documents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub related: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// Replaces the editors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editors: Option<Vec<String>>,
    /// `draft`, `active` or `archived` (`superseded` is set by
    /// `POST /docs/{num}/supersede`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<DocStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned: Option<bool>,
    /// RFC 3339.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub expires_at: Option<Option<String>>,
    /// The revision message when the title or description changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_rev: Option<i64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub silent: bool,
}

/// Body of `POST /v1/docs/{num}/supersede`: `by` replaces the document.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DocSupersede {
    pub by: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub silent: bool,
}

/// Body of `POST /v1/docs/{num}/verify` (optional).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DocVerify {
    /// How long the confirmation holds: `30d`, `12h`, `2w`, or `none`
    /// (default: the kind's interval).
    #[serde(default, rename = "for", skip_serializing_if = "Option::is_none")]
    pub for_: Option<String>,
}

/// Body of `POST /v1/docs/{num}/split`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DocSplit {
    /// The heading level whose sections become documents (1 or 2;
    /// default 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_rev: Option<i64>,
}

/// Body of `POST /v1/docs/{num}/extract`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DocExtract {
    /// The section to move out.
    pub anchor: String,
    /// The new document's title.
    pub into: String,
    /// The new document's kind (default: the source's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_rev: Option<i64>,
}

/// Body of `POST /v1/docs/{num}/merge`: the document `num` is appended to
/// `into` as a section.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DocMerge {
    pub into: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_rev: Option<i64>,
}

/// Body of `POST /v1/docs/{num}/comment`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DocCommentRequest {
    pub body: String,
    /// A section of the current revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
}

/// Answer of `POST /v1/docs/{num}/comment`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocCommentResponse {
    pub id: i64,
    pub thread_id: i64,
    pub doc: i64,
}

/// Answer of `GET /v1/docs`, `/docs/find`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocList {
    pub docs: Vec<Doc>,
    /// Pass as `before` for the next (older) page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_before: Option<i64>,
}

/// One hit of `GET /v1/docs/search`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocSearchHit {
    pub num: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    /// The section (empty: the text before the first heading, or a match in
    /// the title or description).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub anchor: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub path: String,
    pub kind: String,
    pub status: DocStatus,
    pub title: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stale: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_at: Option<String>,
    /// The matching sentence, at most 200 characters.
    pub snippet: String,
    /// The hit as one line: `d12#rollback  runbook  active  Deploy >
    /// Rollback — <snippet>  (verified 2026-10-20)`.
    pub line: String,
}

/// Answer of `GET /v1/docs/search`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocSearchResult {
    pub hits: Vec<DocSearchHit>,
    /// Every term was shorter than 3 characters: titles and descriptions
    /// were searched, not sections.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub titles_only: bool,
}

/// Answer of `GET /v1/docs/index`: what a seat is shown each turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocIndex {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    /// One line per document: `d12  runbook  <title> — <description>`.
    pub lines: Vec<String>,
    /// Documents left out by the budget.
    #[serde(default)]
    pub more: i64,
    /// `… and 41 more: amail doc list --team backend` when `more > 0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub more_line: Option<String>,
    /// The lines and the more line, joined with newlines.
    pub text: String,
}

/// Answer of `GET /v1/docs/{num}/history`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocHistory {
    pub revisions: Vec<DocRevision>,
    pub events: Vec<DocEvent>,
    /// Pass as `after_seq` for the next page of events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_after_seq: Option<i64>,
}

/// Answer of `GET /v1/docs/{num}/diff`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocDiff {
    pub num: i64,
    pub from: i64,
    pub to: i64,
    /// A unified diff of the bodies (title and description changes as a
    /// header).
    pub diff: String,
}

/// Answer of `GET /v1/docs/{num}/links`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocLinks {
    pub links: Vec<DocLink>,
    pub linked_from: Vec<DocLink>,
}

/// Answer of `GET /v1/docs/check`: dangling and stale links.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocCheck {
    pub problems: Vec<DocLink>,
}

/// Body of a 409 `stale` answer of a document write.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocStale {
    pub error: crate::ErrorDetail,
    pub rev: i64,
    pub doc: DocDetail,
    /// Revisions and events after `if_rev`, oldest first.
    pub since: Vec<DocEvent>,
    /// Unified diff from `if_rev` to the current revision.
    pub diff: String,
}

/// A document kind of the org.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocKind {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default)]
    pub required_sections: Vec<String>,
    pub activate_by: ActivateBy,
    pub in_index: bool,
    pub body_cap_bytes: i64,
    /// Default interval of `verify`, in days.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify_days: Option<i64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub builtin: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
    /// Documents of this kind.
    #[serde(default)]
    pub docs: i64,
}

/// Answer of `GET /v1/docs/kinds`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocKindList {
    pub kinds: Vec<DocKind>,
}

/// Body of `POST /v1/docs/kinds` (user token): add a kind, or change one
/// (`disabled` turns off new documents of it).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NewDocKind {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_sections: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activate_by: Option<ActivateBy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_index: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_cap_bytes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify_days: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
}

/// Payload of a server-sent `doc` event (user and seat tokens): a document
/// of the org changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocBusEvent {
    pub num: i64,
    /// `created`, `revised`, `changed`, `relation`, `verified`, `split`,
    /// `extracted`, `merged`, `link`, `commented`, `reminder`.
    pub change: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_round_trip() {
        for s in DocStatus::ALL {
            assert_eq!(s.as_str().parse::<DocStatus>().unwrap(), *s);
        }
    }

    #[test]
    fn patch_null_clears() {
        let p: DocPatch = serde_json::from_str(r#"{"slug":null,"title":"x","extra":1}"#).unwrap();
        assert_eq!(p.slug, Some(None));
        assert_eq!(p.team, None);
    }
}
