//! `amail doc …`: the org's documents (`/v1/docs…`).
//!
//! Each operation is a function that makes the API calls and returns the
//! server's JSON with its text rendering, so that the command line and the
//! MCP tools (`doc_*` in [`crate::mcp`]) print the same lines. Every value
//! that comes from the server goes through [`l`] or [`text::subject`];
//! bodies are printed whole by `read` (the text is made safe by
//! [`crate::Out`]) and behind `  > ` by `show`.

use crate::Out;
use crate::client::{Client, enc};
use crate::output::{l, short_time};
use amail_proto::{self as p, text};
use anyhow::{Context, Result, bail};
use reqwest::Method;
use serde_json::{Map, Value, json};

/// The body characters `show` prints (an `index` document:
/// [`p::limits::DOC_INDEX_BODY_CHARS`]).
pub const SHOW_BODY_CHARS: usize = 2_000;

/// The sections `show` lists before pointing to `read --sections`.
const SHOW_SECTIONS: usize = 30;

/// The events `history` prints without `--all`.
const HISTORY_TAIL: i64 = 50;

#[derive(clap::Subcommand)]
pub enum DocCmd {
    /// Create a document (status draft). Similar existing documents are
    /// printed as a warning; the create is never refused for them.
    New {
        /// One line.
        title: String,
        /// A kind of the org (`amail doc kinds`; default reference).
        #[arg(long)]
        kind: Option<String>,
        /// The body (Markdown).
        #[arg(short = 'm', long, conflicts_with = "file")]
        body: Option<String>,
        /// Read the body from a file (`-`: stdin).
        #[arg(long)]
        file: Option<std::path::PathBuf>,
        /// One line for the index: what the document is and when to read
        /// it (at most 200 characters).
        #[arg(long)]
        description: Option<String>,
        /// An alias accepted wherever dN is (`runbook.deploy.rollback`).
        #[arg(long)]
        slug: Option<String>,
        /// Default: the owner's team.
        #[arg(long)]
        team: Option<String>,
        /// A label (repeat for more).
        #[arg(long)]
        label: Vec<String>,
        /// The parent document (dN or a slug).
        #[arg(long, value_name = "DOC")]
        parent: Option<String>,
        /// The document this one supplements (dN or a slug).
        #[arg(long, value_name = "DOC")]
        supplements: Option<String>,
        /// The revision message.
        #[arg(long)]
        message: Option<String>,
        /// Record the events but send no notification mail.
        #[arg(long)]
        silent: bool,
        /// Create a document even if the same one (you, kind, title,
        /// description) was created in the last 24 hours. Without it,
        /// running the same command again answers the document it created.
        #[arg(long = "new")]
        new: bool,
    },
    /// A document: its fields, relations, links in and out, linked tasks
    /// and threads, the last 20 events, and the start of the body.
    Show {
        /// dN, N or a slug.
        doc: String,
    },
    /// Print the body, one section (dN#anchor), an old revision (--rev),
    /// or the section list with anchors and token estimates (--sections).
    Read {
        /// dN, dN#anchor, or a slug (with #anchor).
        doc: String,
        #[arg(long)]
        rev: Option<i64>,
        /// List the sections instead of printing the text.
        #[arg(long, conflicts_with = "rev")]
        sections: bool,
    },
    /// Replace the body, or one section (--section or dN#anchor). The text
    /// comes from -m, --file or stdin.
    Write {
        /// dN, dN#anchor, or a slug.
        doc: String,
        #[arg(short = 'm', long, conflicts_with = "file")]
        body: Option<String>,
        /// Read the text from a file (`-`: stdin).
        #[arg(long)]
        file: Option<std::path::PathBuf>,
        /// Write only this section (its heading stays unless the text
        /// starts with a heading).
        #[arg(long, value_name = "ANCHOR")]
        section: Option<String>,
        /// The revision message (at most 500 characters).
        #[arg(long)]
        message: Option<String>,
        /// Refuse the write if the document changed since this revision
        /// (`Rev:` of `doc show`).
        #[arg(long, value_name = "REV")]
        if_rev: Option<i64>,
        /// Send no notification mail.
        #[arg(long)]
        silent: bool,
    },
    /// Change fields of a document.
    Set {
        doc: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        description: Option<String>,
        /// A slug, or `none`.
        #[arg(long)]
        slug: Option<String>,
        /// Replaces the labels: comma-separated, or `none`.
        #[arg(long)]
        labels: Option<String>,
        /// A team, or `none`.
        #[arg(long)]
        team: Option<String>,
        /// A document, or `none`.
        #[arg(long, value_name = "DOC")]
        parent: Option<String>,
        /// A document this one supplements, or `none`.
        #[arg(long, value_name = "DOC")]
        supplements: Option<String>,
        /// Replaces the related documents: comma-separated, or `none`.
        #[arg(long, value_name = "DOCS")]
        related: Option<String>,
        /// The owner seat.
        #[arg(long)]
        owner: Option<String>,
        /// Replaces the editors (seats that may revise): comma-separated,
        /// or `none`.
        #[arg(long)]
        editors: Option<String>,
        /// draft, active or archived (supersede with `amail doc
        /// supersede`).
        #[arg(long)]
        status: Option<String>,
        /// Pin the document into the per-turn index of its team.
        #[arg(long, conflicts_with = "unpin")]
        pin: bool,
        #[arg(long)]
        unpin: bool,
        /// The revision message when the title or description changes.
        #[arg(long)]
        message: Option<String>,
        #[arg(long, value_name = "REV")]
        if_rev: Option<i64>,
        #[arg(long)]
        silent: bool,
    },
    /// Mark a document superseded by another one (status and relation in
    /// one call).
    Supersede {
        doc: String,
        /// The document that replaces it.
        #[arg(long, value_name = "DOC")]
        by: String,
        #[arg(long)]
        silent: bool,
    },
    /// Confirm that the content is current: sets verified_at and the next
    /// expiry.
    Verify {
        doc: String,
        /// How long the confirmation holds: 30d, 12h, 2w or none (default:
        /// the kind's interval).
        #[arg(long = "for", value_name = "INTERVAL")]
        for_: Option<String>,
    },
    /// Turn each heading of a level into a child document; links to the
    /// moved sections follow. Prints where each section went.
    Split {
        doc: String,
        /// 1 or 2 (default 1).
        #[arg(long)]
        level: Option<i64>,
        #[arg(long, value_name = "REV")]
        if_rev: Option<i64>,
    },
    /// Move one section (dN#anchor) into a new document that supplements
    /// this one; links follow.
    Extract {
        /// dN#anchor.
        doc: String,
        /// The new document's title.
        #[arg(long, value_name = "TITLE")]
        into: String,
        /// The new document's kind (default: the source's).
        #[arg(long)]
        kind: Option<String>,
        #[arg(long, value_name = "REV")]
        if_rev: Option<i64>,
    },
    /// Append a child or supplement to another document as a section; it
    /// becomes superseded by it and links follow.
    Merge {
        /// The document that is merged.
        doc: String,
        /// The document it is appended to.
        #[arg(long, value_name = "DOC")]
        into: String,
        #[arg(long, value_name = "REV")]
        if_rev: Option<i64>,
    },
    /// Documents of the org, newest first (draft and active unless
    /// --status).
    List {
        #[arg(long)]
        kind: Option<String>,
        /// Comma-separated statuses, or `all`.
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        team: Option<String>,
        #[arg(long)]
        owner: Option<String>,
        #[arg(long)]
        label: Option<String>,
        /// Only documents past their expiry.
        #[arg(long)]
        stale: bool,
        #[arg(long)]
        pinned: bool,
        #[arg(long)]
        limit: Option<u32>,
        /// Continue an earlier listing.
        #[arg(long)]
        before: Option<i64>,
    },
    /// Search the sections of documents (active ones unless --status).
    Search {
        /// The terms (at most 10).
        #[arg(required = true, num_args = 1..)]
        terms: Vec<String>,
        #[arg(long)]
        kind: Option<String>,
        /// A status, or `all`.
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        team: Option<String>,
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        owner: Option<String>,
        #[arg(long)]
        limit: Option<u32>,
    },
    /// Find documents by words of their title, slug or description.
    Find {
        #[arg(required = true, num_args = 1..)]
        words: Vec<String>,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        limit: Option<u32>,
    },
    /// The index a seat is shown each turn.
    Index {
        /// Default: your team.
        #[arg(long)]
        team: Option<String>,
    },
    /// Revisions and events (the last 50 events; every event with --all).
    History {
        doc: String,
        #[arg(long)]
        all: bool,
    },
    /// A unified diff between two revisions (default: the last change;
    /// with one revision, from it to the current one).
    Diff {
        doc: String,
        from: Option<i64>,
        to: Option<i64>,
    },
    /// The links into a document.
    Backlinks { doc: String },
    /// Dangling and stale links of draft and active documents.
    Check {
        #[arg(long)]
        team: Option<String>,
    },
    /// Comment: mail in the document's discussion thread to its owner. The
    /// body comes from -m or stdin; dN#anchor comments on a section.
    Comment {
        doc: String,
        #[arg(short = 'm', long)]
        body: Option<String>,
    },
    /// The document kinds of the org.
    Kinds,
    /// Add or change a document kind (user token).
    Kind {
        #[command(subcommand)]
        cmd: KindCmd,
    },
}

#[derive(clap::Subcommand)]
pub enum KindCmd {
    /// Add a kind, or change one.
    Add {
        name: String,
        /// Headings the body must contain, comma-separated.
        #[arg(long)]
        sections: Option<String>,
        /// owner or human: who moves a document from draft to active.
        #[arg(long, value_name = "WHO")]
        activate_by: Option<String>,
        /// Active documents of this kind are not in the per-turn index.
        #[arg(long)]
        no_index: bool,
        /// The body cap in bytes.
        #[arg(long, value_name = "BYTES")]
        cap: Option<i64>,
        /// One line shown in `amail doc kinds`.
        #[arg(long)]
        description: Option<String>,
        /// The default interval of `verify` in days (0: no expiry).
        #[arg(long, value_name = "DAYS")]
        verify_days: Option<i64>,
        /// Stop new documents of this kind.
        #[arg(long, conflicts_with = "enable")]
        disable: bool,
        /// Allow new documents of this kind again.
        #[arg(long)]
        enable: bool,
    },
}

// ---------------------------------------------------------------------------
// Arguments

/// `d12`, `12`, a slug, each optionally with `#anchor`: the document and
/// the anchor.
pub fn split_ref(s: &str) -> Result<(String, Option<String>)> {
    let s = s.trim();
    let (id, anchor) = match s.split_once('#') {
        Some((a, b)) => (
            a.trim(),
            Some(b.trim().to_string()).filter(|b| !b.is_empty()),
        ),
        None => (s, None),
    };
    if id.is_empty() {
        bail!("{s:?} is not a document: d12, 12 or a slug, optionally with #anchor");
    }
    Ok((id.to_string(), anchor))
}

/// The path of a document (`/docs/d12`, `/docs/<slug>`).
fn path(id: &str) -> String {
    format!("/docs/{}", enc(id))
}

/// The document named by `s`, for a call that does not take a section.
fn doc_only(s: &str) -> Result<String> {
    let (id, anchor) = split_ref(s)?;
    if let Some(a) = anchor {
        bail!("{s:?}: this command takes a document, not a section (#{a})");
    }
    Ok(id)
}

/// `k=v` pairs, encoded.
pub fn query(pairs: &[(&str, Option<String>)]) -> String {
    let q: Vec<String> = pairs
        .iter()
        .filter_map(|(k, v)| v.as_ref().map(|v| format!("{k}={}", enc(v))))
        .collect();
    if q.is_empty() {
        String::new()
    } else {
        format!("?{}", q.join("&"))
    }
}

/// Text from `-m`, a file (`-`: stdin), or stdin.
fn body_text(body: Option<String>, file: Option<&std::path::Path>) -> Result<String> {
    match (body, file) {
        (Some(b), _) => Ok(b),
        (None, Some(f)) if f.as_os_str() == "-" => crate::read_body(None),
        (None, Some(f)) => {
            std::fs::read_to_string(f).with_context(|| format!("reading {}", f.display()))
        }
        (None, None) => crate::read_body(None),
    }
}

/// A comma-separated list; `none` or empty is the empty list.
fn list_arg(s: &str) -> Vec<String> {
    match s.trim() {
        "" | "none" => Vec::new(),
        v => v
            .split(',')
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect(),
    }
}

/// `none` (or empty) as JSON `null`.
fn or_null(s: &str) -> Value {
    match s.trim() {
        "" | "none" => Value::Null,
        v => json!(v),
    }
}

/// The key `doc new` sends by default: the first 32 hex digits of SHA-256
/// over the caller, kind, title and description.
pub fn default_client_key(caller: &str, kind: &str, title: &str, description: &str) -> String {
    use sha2::Digest;
    let v = json!([caller, kind, title, description]);
    let digest = sha2::Sha256::digest(v.to_string().as_bytes());
    hex::encode(digest)[..32].to_string()
}

// ---------------------------------------------------------------------------
// Rendering

/// `12345` -> `12,345`.
pub fn thousands(n: i64) -> String {
    let s = n.abs().to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

/// One document on one line: number, kind, status, owner, title, the
/// description, and marks.
pub fn doc_line(d: &p::Doc) -> String {
    let mut s = format!(
        "{:<5} {:<9} {:<10} {:<8} {}",
        format!("d{}", d.num),
        l(&d.kind),
        d.status.as_str(),
        l(&d.owner),
        text::subject(&d.title)
    );
    if !d.description.is_empty() {
        s.push_str(&format!(" — {}", l(&d.description)));
    }
    if d.stale {
        s.push_str("  · stale");
    }
    if d.pinned {
        s.push_str("  · pinned");
    }
    if let Some(x) = d.superseded_by {
        s.push_str(&format!("  · superseded by d{x}"));
    }
    s
}

/// A document named from another one.
/// What `amail thread` adds about documents: a mark above each comment
/// whose section the document no longer has, by message id, and the list of
/// documents the thread cites.
pub fn thread_comment_mark(t: &p::ThreadResponse, message_id: i64) -> Option<String> {
    let c = t
        .doc_comments
        .iter()
        .find(|c| c.message_id == message_id && c.removed)?;
    let was = match (&c.path, &c.anchor) {
        (Some(p), _) => l(p).into_owned(),
        (None, Some(a)) => format!("#{}", l(a)),
        (None, None) => return None,
    };
    Some(format!(
        "[comment on a removed section of d{} (was: {was})]",
        c.doc
    ))
}

/// `documents of this thread:` and one line per document.
pub fn thread_docs_text(refs: &[p::DocRef]) -> Option<String> {
    (!refs.is_empty()).then(|| {
        let mut s = String::from("documents of this thread:");
        for r in refs {
            s.push_str(&format!("\n  {}", ref_line(r)));
        }
        s
    })
}

pub fn ref_line(r: &p::DocRef) -> String {
    let mut s = format!(
        "{:<5} {:<9} {:<10} {}",
        format!("d{}", r.num),
        l(&r.kind),
        r.status.as_str(),
        text::subject(&r.title)
    );
    if r.stale {
        s.push_str("  · stale");
    }
    s
}

fn task_ref_line(r: &p::TaskRef) -> String {
    format!(
        "{:<4} {:<7} {:<8} {}",
        format!("t{}", r.num),
        r.state.as_str(),
        r.assignee
            .as_deref()
            .map(|a| l(a).into_owned())
            .unwrap_or("-".into()),
        text::subject(&r.title)
    )
}

fn link_marks(k: &p::DocLink) -> String {
    let mut s = String::new();
    if k.renamed
        && let Some(w) = &k.title_at_write
    {
        s.push_str(&format!("  (renamed; was {})", text::subject(w)));
    }
    if k.dangling {
        s.push_str("  (dangling)");
    }
    if k.stale {
        s.push_str("  (stale: the path no longer exists)");
    }
    s
}

/// A link out of a document: `#steps -> d7#x  Title  (dangling)`.
pub fn link_line(k: &p::DocLink) -> String {
    let mut s = String::new();
    if !k.src_anchor.is_empty() {
        s.push_str(&format!("#{} -> ", l(&k.src_anchor)));
    }
    s.push_str(&l(&k.target));
    if let Some(t) = &k.title {
        s.push_str(&format!("  {}", text::subject(t)));
    }
    s.push_str(&link_marks(k));
    s
}

/// A link into a document: `d40#steps -> d12#rollback`.
pub fn backlink_line(k: &p::DocLink) -> String {
    let mut s = format!("d{}", k.src);
    if !k.src_anchor.is_empty() {
        s.push_str(&format!("#{}", l(&k.src_anchor)));
    }
    s.push_str(&format!(" -> {}", l(&k.target)));
    s.push_str(&link_marks(k));
    s
}

/// A link reported by `check`: `d12#steps -> d99  (dangling)`.
fn problem_line(k: &p::DocLink) -> String {
    backlink_line(k)
}

/// `2026-10-26 10:00Z  w1  revised rev 3 (message)`.
pub fn event_line(e: &p::DocEvent) -> String {
    let actor = match (&e.actor, &e.written_by) {
        (Some(a), Some(w)) => format!("{} (by {})", l(a), l(w)),
        (Some(a), None) => l(a).into_owned(),
        (None, _) => "server".into(),
    };
    let mut what = e.kind.to_string();
    if let Some(f) = &e.field {
        what.push_str(&format!(" {}", l(f)));
    }
    match (&e.from, &e.to) {
        (Some(f), Some(t)) => what.push_str(&format!(": {} -> {}", l(f), l(t))),
        (None, Some(t)) => what.push_str(&format!(": {}", l(t))),
        (Some(f), None) => what.push_str(&format!(": {} -> (none)", l(f))),
        (None, None) => {}
    }
    if let Some(r) = e.rev {
        what.push_str(&format!(" rev {r}"));
    }
    if let Some(m) = e.message_id {
        what.push_str(&format!(" (#{m})"));
    }
    if let Some(n) = &e.note {
        what.push_str(&format!(" ({})", l(n)));
    }
    format!("{}  {actor}  {what}", short_time(&e.at))
}

/// One section: `  #rollback  Deploy > Rollback  ~120 tokens`, indented by
/// its level.
pub fn section_line(s: &p::DocSection) -> String {
    let indent = "  ".repeat(s.level.clamp(1, 6) as usize);
    let anchor = if s.anchor.is_empty() {
        "(before the first heading)".to_string()
    } else {
        format!("#{}", l(&s.anchor))
    };
    let path = if s.path.is_empty() {
        String::new()
    } else {
        format!("  {}", l(&s.path))
    };
    format!("{indent}{anchor}{path}  ~{} tokens", thousands(s.tokens))
}

/// `d12 rev 3 · Title · ~1,234 tokens`.
fn read_header(num: i64, anchor: Option<&str>, rev: i64, what: &str, tokens: i64) -> String {
    let a = anchor.map(|a| format!("#{}", l(a))).unwrap_or_default();
    format!(
        "d{num}{a} rev {rev} · {} · ~{} tokens",
        text::subject(what),
        thousands(tokens)
    )
}

/// The section list of a document.
pub fn sections_text(d: &p::DocDetail) -> String {
    let doc = &d.doc;
    let mut out = vec![format!(
        "{} · {} sections",
        read_header(doc.num, None, doc.rev, &doc.title, doc.tokens),
        d.sections.len()
    )];
    if d.sections.is_empty() {
        out.push("(no headings: read the whole document)".into());
    }
    out.extend(d.sections.iter().map(section_line));
    out.push(format!("read one: amail doc read d{}#<anchor>", doc.num));
    out.join("\n")
}

/// What `read` prints: a header line, then the text.
pub fn read_text(d: &p::DocDetail) -> String {
    let doc = &d.doc;
    let (header, body) = match &d.read {
        Some(r) => (
            read_header(
                doc.num,
                r.anchor.as_deref(),
                r.rev,
                r.path.as_deref().unwrap_or(&r.title),
                r.tokens,
            ),
            r.text.as_str(),
        ),
        None => (
            read_header(doc.num, None, doc.rev, &doc.title, doc.tokens),
            doc.body.as_str(),
        ),
    };
    if body.trim().is_empty() {
        format!("{header}\n(empty)")
    } else {
        format!("{header}\n\n{}", body.trim_end())
    }
}

/// The record, one field per line, relations, links, tasks, threads,
/// comments, the last events, and the start of the body behind `  > `.
pub fn detail_text(d: &p::DocDetail) -> String {
    let t = &d.doc;
    let mut out = vec![format!("d{} {}", t.num, l(&t.title))];
    if let Some(s) = &t.slug {
        out.push(format!("Slug: {}", l(s)));
    }
    let mut status = format!("Kind: {} · Status: {}", l(&t.kind), t.status);
    if t.stale {
        status.push_str(" (stale: verify it with amail doc verify)");
    }
    out.push(status);
    out.push(format!(
        "Rev: {} · ~{} tokens · {} bytes",
        t.rev,
        thousands(t.tokens),
        thousands(t.bytes)
    ));
    if !t.description.is_empty() {
        out.push(format!("Description: {}", l(&t.description)));
    }
    let mut who = format!("Owner: {}", l(&t.owner));
    if let Some(x) = &t.team {
        who.push_str(&format!(" · Team: {}", l(x)));
    }
    out.push(who);
    if !t.editors.is_empty() {
        let v: Vec<String> = t.editors.iter().map(|x| l(x).into_owned()).collect();
        out.push(format!("Editors: {}", v.join(", ")));
    }
    if !t.labels.is_empty() {
        let v: Vec<String> = t.labels.iter().map(|x| l(x).into_owned()).collect();
        out.push(format!("Labels: {}", v.join(", ")));
    }
    if t.pinned {
        out.push("Pinned: yes".into());
    }
    match (&t.verified_at, &t.expires_at) {
        (Some(v), e) => {
            let mut s = format!("Verified: {}", short_time(v));
            if let Some(b) = &t.verified_by {
                s.push_str(&format!(" by {}", l(b)));
            }
            if let Some(e) = e {
                s.push_str(&format!(" · expires {}", short_time(e)));
            }
            out.push(s);
        }
        (None, Some(e)) => out.push(format!("Expires: {}", short_time(e))),
        (None, None) => {}
    }
    if let Some(a) = &t.advice {
        out.push(format!("Advice: {}", l(a)));
    }
    if let Some(x) = t.thread {
        out.push(format!("Discussion: thread {x}"));
    }
    out.push(format!(
        "Created: {} · updated {}",
        short_time(&t.created_at),
        short_time(&t.updated_at)
    ));
    let one = |name: &str, r: &Option<p::DocRef>, out: &mut Vec<String>| {
        if let Some(r) = r {
            out.push(format!("{name}: {}", ref_line(r)));
        }
    };
    one("Parent", &d.parent, &mut out);
    one("Superseded by", &d.superseded_by, &mut out);
    one("Supplements", &d.supplements, &mut out);
    for (name, refs) in [
        ("Children", &d.children),
        ("Replaces", &d.replaces),
        ("Supplemented by", &d.supplemented_by),
        ("Related", &d.related),
    ] {
        if !refs.is_empty() {
            out.push(format!("{name}:"));
            out.extend(refs.iter().map(|r| format!("  {}", ref_line(r))));
        }
    }
    if !d.sections.is_empty() {
        out.push(format!(
            "Sections ({}; read one: amail doc read d{}#<anchor>):",
            d.sections.len(),
            t.num
        ));
        out.extend(d.sections.iter().take(SHOW_SECTIONS).map(section_line));
        if d.sections.len() > SHOW_SECTIONS {
            out.push(format!(
                "  ... and {} more: amail doc read d{} --sections",
                d.sections.len() - SHOW_SECTIONS,
                t.num
            ));
        }
    }
    if !d.links.is_empty() {
        out.push("Links:".into());
        out.extend(d.links.iter().map(|k| format!("  {}", link_line(k))));
    }
    if !d.linked_from.is_empty() {
        out.push("Linked from:".into());
        out.extend(
            d.linked_from
                .iter()
                .map(|k| format!("  {}", backlink_line(k))),
        );
    }
    if !d.tasks.is_empty() {
        out.push("Tasks:".into());
        out.extend(d.tasks.iter().map(|r| format!("  {}", task_ref_line(r))));
    }
    if !d.threads.is_empty() {
        out.push("Threads:".into());
        for k in &d.threads {
            let by = k
                .linked_by
                .as_deref()
                .map(|b| format!(" by {}", l(b)))
                .unwrap_or_default();
            out.push(format!(
                "  thread {} (linked{by} {})",
                k.thread_id,
                short_time(&k.linked_at)
            ));
        }
    }
    if !d.comments.is_empty() {
        out.push("Comments:".into());
        for c in &d.comments {
            let by =
                c.by.as_deref()
                    .map(|b| format!(" by {}", l(b)))
                    .unwrap_or_default();
            let on = match (&c.anchor, &c.path, c.removed) {
                (_, Some(p), true) => format!(" on a removed section (was: {})", l(p)),
                (Some(a), _, true) => format!(" on a removed section (was: #{})", l(a)),
                (Some(a), Some(p), false) => format!(" on #{} ({})", l(a), l(p)),
                (Some(a), None, false) => format!(" on #{}", l(a)),
                _ => String::new(),
            };
            out.push(format!("  #{}{by} {}{on}", c.message_id, short_time(&c.at)));
        }
    }
    if !d.events.is_empty() {
        out.push(if d.event_count > d.events.len() as i64 {
            format!(
                "Events (last {} of {}; all: amail doc history d{} --all):",
                d.events.len(),
                d.event_count,
                t.num
            )
        } else {
            "Events:".into()
        });
        out.extend(d.events.iter().map(|e| format!("  {}", event_line(e))));
    }
    if !t.body.is_empty() {
        let max = if t.kind == "index" {
            p::limits::DOC_INDEX_BODY_CHARS
        } else {
            SHOW_BODY_CHARS
        };
        out.push(String::new());
        let n = t.body.chars().count();
        match t.body.char_indices().nth(max) {
            None => out.push(text::quote(&t.body).trim_end().to_string()),
            Some((i, _)) => {
                out.push(text::quote(&t.body[..i]).trim_end().to_string());
                out.push(format!(
                    "body not shown: amail doc read d{} (the first {} of {} characters are above)",
                    t.num,
                    thousands(max as i64),
                    thousands(n as i64)
                ));
            }
        }
    }
    out.join("\n")
}

/// The answer of a write: the document's line, the revision, and what the
/// write found (an unchanged body, dangling links, advice, candidates).
pub fn written_text(d: &p::DocDetail) -> String {
    let t = &d.doc;
    let mut out = vec![doc_line(t)];
    if d.existing {
        out.push(
            "(created before by the same command; nothing new was created: amail doc new --new creates another)"
                .into(),
        );
    }
    if d.unchanged {
        out.push(format!(
            "unchanged: the text equals rev {}; no revision was written",
            t.rev
        ));
    } else {
        out.push(format!("rev {} · ~{} tokens", t.rev, thousands(t.tokens)));
    }
    if let Some(c) = &d.candidates_line {
        out.push(format!("warning: {}", l(c)));
    }
    if !d.dangling.is_empty() {
        let v: Vec<String> = d
            .dangling
            .iter()
            .map(|k| l(&k.target).into_owned())
            .collect();
        out.push(format!(
            "dangling links (the write was kept): {}",
            v.join(", ")
        ));
    }
    if let Some(a) = &t.advice {
        out.push(format!("advice: {}", l(a)));
    }
    out.join("\n")
}

/// The answer of `split`, `extract` and `merge`: where each section went.
pub fn moved_text(d: &p::DocDetail, verb: &str) -> String {
    let mut out = vec![format!("d{} {verb}:", d.doc.num)];
    if d.moved.is_empty() {
        out.push("  nothing moved".into());
    }
    out.extend(
        d.moved
            .iter()
            .map(|m| format!("  {} -> {}", l(&m.from), l(&m.to))),
    );
    out.push(doc_line(&d.doc));
    out.join("\n")
}

/// The text of a 409 `stale` answer: the revisions and events since the
/// caller's revision, and the diff to the current one. `retry` is the last
/// line (how to try again).
pub fn stale_text(s: &p::DocStale, retry: &str) -> String {
    let mut out = vec![format!(
        "d{} changed since your revision; it is now at rev {}.",
        s.doc.doc.num, s.rev
    )];
    if !s.since.is_empty() {
        out.push("Changes since then:".into());
        out.extend(s.since.iter().map(|e| format!("  {}", event_line(e))));
    }
    if !s.diff.trim().is_empty() {
        out.push(format!("Diff to rev {}:", s.rev));
        out.push(s.diff.trim_end().to_string());
    }
    out.push(String::new());
    out.push(format!(
        "Nothing was changed. Merge your change with the diff above, then {retry} {}.",
        s.rev
    ));
    out.join("\n")
}

fn docs_text(docs: &[p::Doc], next: Option<i64>, empty: &str) -> String {
    let mut lines: Vec<String> = docs.iter().map(doc_line).collect();
    if lines.is_empty() {
        lines.push(empty.into());
    }
    if let Some(b) = next {
        lines.push(format!("(older: amail doc list --before {b} ...)"));
    }
    lines.join("\n")
}

pub fn search_text(r: &p::DocSearchResult) -> String {
    let mut lines: Vec<String> = r.hits.iter().map(|h| l(&h.line).into_owned()).collect();
    if lines.is_empty() {
        lines.push("no hits".into());
    }
    if r.titles_only {
        lines.push(
            "(every term is shorter than 3 characters: titles and descriptions were searched)"
                .into(),
        );
    }
    lines.join("\n")
}

pub fn index_text(r: &p::DocIndex) -> String {
    if r.text.trim().is_empty() {
        return "no documents in the index".into();
    }
    r.text
        .lines()
        .map(|x| l(x).into_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

fn revision_line(r: &p::DocRevision) -> String {
    let who = match (&r.author, &r.written_by) {
        (Some(a), Some(w)) => format!("{} (by {})", l(a), l(w)),
        (Some(a), None) => l(a).into_owned(),
        (None, _) => "-".into(),
    };
    let mut s = format!(
        "rev {:<3} {}  {who}  {}  {} bytes",
        r.rev,
        short_time(&r.at),
        text::subject(&r.title),
        thousands(r.bytes)
    );
    if let Some(m) = &r.message {
        s.push_str(&format!("  ({})", l(m)));
    }
    s
}

/// Revisions, then events; `earlier` is the number of events left out.
pub fn history_text(h: &p::DocHistory, num: Option<i64>, earlier: i64) -> String {
    let mut out = vec!["Revisions:".to_string()];
    out.extend(
        h.revisions
            .iter()
            .map(|r| format!("  {}", revision_line(r))),
    );
    out.push("Events:".into());
    if earlier > 0 {
        let d = num.map(|n| format!("d{n}")).unwrap_or("dN".into());
        out.push(format!(
            "  ({earlier} earlier events: amail doc history {d} --all)"
        ));
    }
    out.extend(h.events.iter().map(|e| format!("  {}", event_line(e))));
    if let Some(s) = h.next_after_seq {
        out.push(format!("  (more: after_seq={s})"));
    }
    out.join("\n")
}

pub fn kind_line(k: &p::DocKind) -> String {
    let mut s = format!(
        "{:<10} activate by {:<5}  {}  verify {}  cap {} bytes  {} docs",
        l(&k.name),
        k.activate_by.as_str(),
        if k.in_index {
            "in index"
        } else {
            "not in index"
        },
        k.verify_days
            .map(|d| format!("{d}d"))
            .unwrap_or("none".into()),
        thousands(k.body_cap_bytes),
        k.docs
    );
    if !k.required_sections.is_empty() {
        let v: Vec<String> = k
            .required_sections
            .iter()
            .map(|x| l(x).into_owned())
            .collect();
        s.push_str(&format!("  sections: {}", v.join(", ")));
    }
    if k.disabled {
        s.push_str("  (disabled)");
    }
    if !k.description.is_empty() {
        s.push_str(&format!(" — {}", l(&k.description)));
    }
    s
}

// ---------------------------------------------------------------------------
// Operations, shared with the MCP tools: each answers the server's JSON and
// its text.

pub type Answer = (Value, String);

/// `POST /docs`. Without a `client_key` in `body` and with `derive_key`,
/// sends the default key.
pub async fn create(c: &Client, mut body: Map<String, Value>, derive_key: bool) -> Result<Answer> {
    if derive_key && !body.contains_key("client_key") {
        let me = crate::task_cmd::reporter_name(c).await?;
        let s = |k: &str| {
            body.get(k)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        let kind = Some(s("kind"))
            .filter(|k| !k.is_empty())
            .unwrap_or("reference".into());
        let key = default_client_key(&me, &kind, &s("title"), &s("description"));
        body.insert("client_key".into(), json!(key));
    }
    let v = c
        .raw(Method::POST, "/docs", Some(&Value::Object(body)))
        .await?;
    let d: p::DocDetail = crate::parse(&v)?;
    Ok((v, written_text(&d)))
}

pub async fn show(c: &Client, doc: &str) -> Result<Answer> {
    let id = doc_only(doc)?;
    let v = c.raw(Method::GET, &path(&id), None).await?;
    let t = detail_text(&crate::parse(&v)?);
    Ok((v, t))
}

/// `read`: the whole body, a section (`dN#anchor` or `section`), a
/// revision, or with `sections` the section list. With `whole_max`, a
/// whole body above that many characters is answered with the section
/// list instead (the MCP tool's rule).
pub async fn read(
    c: &Client,
    doc: &str,
    section: Option<String>,
    rev: Option<i64>,
    sections: bool,
    whole_max: Option<usize>,
) -> Result<Answer> {
    let (id, anchor) = split_ref(doc)?;
    let section = section
        .map(|s| s.trim().trim_start_matches('#').to_string())
        .filter(|s| !s.is_empty())
        .or(anchor);
    if sections {
        let v = c.raw(Method::GET, &path(&id), None).await?;
        let d: p::DocDetail = crate::parse(&v)?;
        return Ok((v, sections_text(&d)));
    }
    let q = query(&[
        ("section", section.clone()),
        ("rev", rev.map(|r| r.to_string())),
    ]);
    let v = c
        .raw(Method::GET, &format!("{}{q}", path(&id)), None)
        .await?;
    let d: p::DocDetail = crate::parse(&v)?;
    if let Some(max) = whole_max {
        let chars = d
            .read
            .as_ref()
            .map(|r| r.text.chars().count())
            .unwrap_or_else(|| d.doc.body.chars().count());
        if section.is_none() && chars > max {
            let mut t = sections_text(&d);
            t.push_str(&format!(
                "\nThe document is {} characters, more than {}: read one section (doc \"d{}#<anchor>\"), or pass whole: true.",
                thousands(chars as i64),
                thousands(max as i64),
                d.doc.num
            ));
            let mut v = v;
            if let Some(o) = v.get_mut("doc").and_then(Value::as_object_mut) {
                o.remove("body");
            }
            if let Some(o) = v.as_object_mut() {
                o.remove("read");
                o.insert("whole_chars".into(), json!(chars));
            }
            return Ok((v, t));
        }
    }
    Ok((v, read_text(&d)))
}

/// `PUT /docs/{doc}/body`; `body` holds `body` and optionally `section`,
/// `message`, `if_rev`, `silent`. A `dN#anchor` names the section.
pub async fn write(c: &Client, doc: &str, mut body: Map<String, Value>) -> Result<Answer> {
    let (id, anchor) = split_ref(doc)?;
    if let Some(a) = anchor
        && !body.contains_key("section")
    {
        body.insert("section".into(), json!(a));
    }
    if let Some(Value::String(s)) = body.get_mut("section") {
        *s = s.trim().trim_start_matches('#').to_string();
    }
    let v = c
        .raw(
            Method::PUT,
            &format!("{}/body", path(&id)),
            Some(&Value::Object(body)),
        )
        .await?;
    let d: p::DocDetail = crate::parse(&v)?;
    Ok((v, written_text(&d)))
}

/// `PATCH /docs/{doc}`.
pub async fn set(c: &Client, doc: &str, body: Map<String, Value>) -> Result<Answer> {
    let id = doc_only(doc)?;
    if body
        .keys()
        .all(|k| matches!(k.as_str(), "if_rev" | "silent" | "message"))
    {
        bail!("nothing to change: give at least one field");
    }
    let v = c
        .raw(Method::PATCH, &path(&id), Some(&Value::Object(body)))
        .await?;
    let d: p::DocDetail = crate::parse(&v)?;
    Ok((v, written_text(&d)))
}

pub async fn supersede(c: &Client, doc: &str, by: &str, silent: bool) -> Result<Answer> {
    let id = doc_only(doc)?;
    let by = doc_only(by)?;
    let v = c
        .raw(
            Method::POST,
            &format!("{}/supersede", path(&id)),
            Some(&json!({ "by": by, "silent": silent })),
        )
        .await?;
    let d: p::DocDetail = crate::parse(&v)?;
    Ok((v, doc_line(&d.doc)))
}

pub async fn verify(c: &Client, doc: &str, for_: Option<String>) -> Result<Answer> {
    let id = doc_only(doc)?;
    let body = match for_ {
        Some(f) => json!({ "for": f }),
        None => json!({}),
    };
    let v = c
        .raw(Method::POST, &format!("{}/verify", path(&id)), Some(&body))
        .await?;
    let d: p::DocDetail = crate::parse(&v)?;
    let t = &d.doc;
    let mut s = format!("d{} verified", t.num);
    if let Some(b) = &t.verified_by {
        s.push_str(&format!(" by {}", l(b)));
    }
    if let Some(a) = &t.verified_at {
        s.push_str(&format!(" at {}", short_time(a)));
    }
    match &t.expires_at {
        Some(e) => s.push_str(&format!("; expires {}", short_time(e))),
        None => s.push_str("; no expiry"),
    }
    Ok((v, s))
}

pub async fn split(
    c: &Client,
    doc: &str,
    level: Option<i64>,
    if_rev: Option<i64>,
) -> Result<Answer> {
    let id = doc_only(doc)?;
    let mut b = Map::new();
    if let Some(x) = level {
        b.insert("level".into(), json!(x));
    }
    if let Some(x) = if_rev {
        b.insert("if_rev".into(), json!(x));
    }
    let v = c
        .raw(
            Method::POST,
            &format!("{}/split", path(&id)),
            Some(&Value::Object(b)),
        )
        .await?;
    let d: p::DocDetail = crate::parse(&v)?;
    let n = d.moved.len();
    Ok((v, moved_text(&d, &format!("split into {n} documents"))))
}

pub async fn extract(
    c: &Client,
    doc: &str,
    anchor: Option<String>,
    into: &str,
    kind: Option<String>,
    if_rev: Option<i64>,
) -> Result<Answer> {
    let (id, a) = split_ref(doc)?;
    let Some(anchor) = anchor
        .map(|s| s.trim().trim_start_matches('#').to_string())
        .filter(|s| !s.is_empty())
        .or(a)
    else {
        bail!("name the section: amail doc extract d12#<anchor> --into <title>");
    };
    let mut b = json!({ "anchor": anchor, "into": into });
    if let Some(k) = kind {
        b["kind"] = json!(k);
    }
    if let Some(r) = if_rev {
        b["if_rev"] = json!(r);
    }
    let v = c
        .raw(Method::POST, &format!("{}/extract", path(&id)), Some(&b))
        .await?;
    let d: p::DocDetail = crate::parse(&v)?;
    Ok((v, moved_text(&d, "section moved out")))
}

pub async fn merge(c: &Client, doc: &str, into: &str, if_rev: Option<i64>) -> Result<Answer> {
    let id = doc_only(doc)?;
    let into = doc_only(into)?;
    let mut b = json!({ "into": into });
    if let Some(r) = if_rev {
        b["if_rev"] = json!(r);
    }
    let v = c
        .raw(Method::POST, &format!("{}/merge", path(&id)), Some(&b))
        .await?;
    let d: p::DocDetail = crate::parse(&v)?;
    Ok((v, moved_text(&d, "merged")))
}

pub async fn list(c: &Client, q: &[(&str, Option<String>)]) -> Result<Answer> {
    let v = c
        .raw(Method::GET, &format!("/docs{}", query(q)), None)
        .await?;
    let r: p::DocList = crate::parse(&v)?;
    Ok((v, docs_text(&r.docs, r.next_before, "no documents")))
}

pub async fn search(c: &Client, q: &[(&str, Option<String>)]) -> Result<Answer> {
    let v = c
        .raw(Method::GET, &format!("/docs/search{}", query(q)), None)
        .await?;
    let r: p::DocSearchResult = crate::parse(&v)?;
    Ok((v, search_text(&r)))
}

pub async fn find(c: &Client, q: &[(&str, Option<String>)]) -> Result<Answer> {
    let v = c
        .raw(Method::GET, &format!("/docs/find{}", query(q)), None)
        .await?;
    let r: p::DocList = crate::parse(&v)?;
    Ok((v, docs_text(&r.docs, None, "no documents")))
}

pub async fn index(c: &Client, team: Option<String>) -> Result<Answer> {
    let v = c
        .raw(
            Method::GET,
            &format!("/docs/index{}", query(&[("team", team)])),
            None,
        )
        .await?;
    let r: p::DocIndex = crate::parse(&v)?;
    Ok((v, index_text(&r)))
}

/// One page of the history (`after_seq`, `limit`).
pub async fn history_page(
    c: &Client,
    doc: &str,
    after_seq: Option<i64>,
    limit: Option<i64>,
) -> Result<Answer> {
    let id = doc_only(doc)?;
    let q = query(&[
        ("after_seq", after_seq.map(|x| x.to_string())),
        ("limit", limit.map(|x| x.to_string())),
    ]);
    let v = c
        .raw(Method::GET, &format!("{}/history{q}", path(&id)), None)
        .await?;
    let h: p::DocHistory = crate::parse(&v)?;
    Ok((v, history_text(&h, None, 0)))
}

/// `history`: every revision and the last 50 events, or with `all` every
/// event (read page by page).
pub async fn history(c: &Client, doc: &str, all: bool) -> Result<Answer> {
    let id = doc_only(doc)?;
    let base = format!("{}/history", path(&id));
    if all {
        let mut events: Vec<Value> = Vec::new();
        let mut after = 0;
        let revisions = loop {
            let page = c
                .raw(
                    Method::GET,
                    &format!("{base}?after_seq={after}&limit={}", p::limits::MAX_DOC_PAGE),
                    None,
                )
                .await?;
            if let Some(e) = page["events"].as_array() {
                events.extend(e.iter().cloned());
            }
            match page["next_after_seq"].as_i64() {
                Some(a) if a > after => after = a,
                _ => break page["revisions"].clone(),
            }
        };
        let v = json!({ "revisions": revisions, "events": events });
        let h: p::DocHistory = crate::parse(&v)?;
        let t = history_text(&h, None, 0);
        return Ok((v, t));
    }
    let d: p::DocDetail = c.get(&path(&id)).await?;
    let after = (d.event_count - HISTORY_TAIL).max(0);
    let v = c
        .raw(
            Method::GET,
            &format!("{base}?after_seq={after}&limit={HISTORY_TAIL}"),
            None,
        )
        .await?;
    let h: p::DocHistory = crate::parse(&v)?;
    Ok((v, history_text(&h, Some(d.doc.num), after)))
}

pub async fn diff(c: &Client, doc: &str, from: Option<i64>, to: Option<i64>) -> Result<Answer> {
    let id = doc_only(doc)?;
    let q = query(&[
        ("from", from.map(|x| x.to_string())),
        ("to", to.map(|x| x.to_string())),
    ]);
    let v = c
        .raw(Method::GET, &format!("{}/diff{q}", path(&id)), None)
        .await?;
    let r: p::DocDiff = crate::parse(&v)?;
    let t = if r.diff.trim().is_empty() {
        format!(
            "d{}: no differences between rev {} and rev {}",
            r.num, r.from, r.to
        )
    } else {
        r.diff.trim_end().to_string()
    };
    Ok((v, t))
}

pub async fn backlinks(c: &Client, doc: &str) -> Result<Answer> {
    let id = doc_only(doc)?;
    let v = c
        .raw(Method::GET, &format!("{}/links", path(&id)), None)
        .await?;
    let r: p::DocLinks = crate::parse(&v)?;
    let t = if r.linked_from.is_empty() {
        format!("no links to {id}")
    } else {
        crate::lines(&r.linked_from, backlink_line)
    };
    Ok((v, t))
}

pub async fn check(c: &Client, team: Option<String>) -> Result<Answer> {
    let v = c
        .raw(
            Method::GET,
            &format!("/docs/check{}", query(&[("team", team)])),
            None,
        )
        .await?;
    let r: p::DocCheck = crate::parse(&v)?;
    let t = if r.problems.is_empty() {
        "no dangling or stale links".to_string()
    } else {
        crate::lines(&r.problems, problem_line)
    };
    Ok((v, t))
}

pub async fn comment(
    c: &Client,
    doc: &str,
    anchor: Option<String>,
    body: String,
) -> Result<Answer> {
    let (id, a) = split_ref(doc)?;
    let anchor = anchor
        .map(|s| s.trim().trim_start_matches('#').to_string())
        .filter(|s| !s.is_empty())
        .or(a);
    let mut b = json!({ "body": body });
    if let Some(a) = anchor {
        b["anchor"] = json!(a);
    }
    let v = c
        .raw(Method::POST, &format!("{}/comment", path(&id)), Some(&b))
        .await?;
    let r: p::DocCommentResponse = crate::parse(&v)?;
    Ok((
        v,
        format!("commented on d{}: #{} thread {}", r.doc, r.id, r.thread_id),
    ))
}

pub async fn kinds(c: &Client) -> Result<Answer> {
    let v = c.raw(Method::GET, "/docs/kinds", None).await?;
    let r: p::DocKindList = crate::parse(&v)?;
    Ok((v, crate::lines(&r.kinds, kind_line)))
}

// ---------------------------------------------------------------------------
// The command line

/// Run a document command; a 409 `stale` prints the changes since and the
/// diff (with `--json`, the whole answer) before the error.
pub async fn run(c: &Client, out: &Out, cmd: DocCmd) -> Result<()> {
    let r = run_cmd(c, cmd).await;
    match r {
        Ok((v, t)) => out.show(&v, || Ok(t)),
        Err(e) => {
            if let Some(api) = e.downcast_ref::<crate::client::ApiError>()
                && api.body.error.code == "stale"
                && let Ok(s) = serde_json::from_value::<p::DocStale>(api.raw.clone())
            {
                out.show(&api.raw, || {
                    Ok(stale_text(&s, "run the command again with --if-rev"))
                })?;
            }
            Err(e)
        }
    }
}

fn put(b: &mut Map<String, Value>, k: &str, v: Option<Value>) {
    if let Some(v) = v {
        b.insert(k.into(), v);
    }
}

async fn run_cmd(c: &Client, cmd: DocCmd) -> Result<Answer> {
    match cmd {
        DocCmd::New {
            title,
            kind,
            body,
            file,
            description,
            slug,
            team,
            label,
            parent,
            supplements,
            message,
            silent,
            new,
        } => {
            let body = match (body, file) {
                (None, None) => None,
                (b, f) => Some(body_text(b, f.as_deref())?),
            };
            let mut b = Map::new();
            put(&mut b, "title", Some(json!(title)));
            put(&mut b, "kind", kind.map(Value::from));
            put(&mut b, "body", body.map(Value::from));
            put(&mut b, "description", description.map(Value::from));
            put(&mut b, "slug", slug.map(Value::from));
            put(&mut b, "team", team.map(Value::from));
            put(&mut b, "labels", (!label.is_empty()).then(|| json!(label)));
            put(&mut b, "parent", parent.map(Value::from));
            put(&mut b, "supplements", supplements.map(Value::from));
            put(&mut b, "message", message.map(Value::from));
            put(&mut b, "silent", silent.then_some(json!(true)));
            create(c, b, !new).await
        }
        DocCmd::Show { doc } => show(c, &doc).await,
        DocCmd::Read { doc, rev, sections } => read(c, &doc, None, rev, sections, None).await,
        DocCmd::Write {
            doc,
            body,
            file,
            section,
            message,
            if_rev,
            silent,
        } => {
            let text = body_text(body, file.as_deref())?;
            let mut b = Map::new();
            put(&mut b, "body", Some(json!(text)));
            put(&mut b, "section", section.map(Value::from));
            put(&mut b, "message", message.map(Value::from));
            put(&mut b, "if_rev", if_rev.map(Value::from));
            put(&mut b, "silent", silent.then_some(json!(true)));
            write(c, &doc, b).await
        }
        DocCmd::Set {
            doc,
            title,
            description,
            slug,
            labels,
            team,
            parent,
            supplements,
            related,
            owner,
            editors,
            status,
            pin,
            unpin,
            message,
            if_rev,
            silent,
        } => {
            let mut b = Map::new();
            put(&mut b, "title", title.map(Value::from));
            put(&mut b, "description", description.map(Value::from));
            put(&mut b, "slug", slug.as_deref().map(or_null));
            put(
                &mut b,
                "labels",
                labels.as_deref().map(|x| json!(list_arg(x))),
            );
            put(&mut b, "team", team.as_deref().map(or_null));
            put(&mut b, "parent", parent.as_deref().map(or_null));
            put(&mut b, "supplements", supplements.as_deref().map(or_null));
            put(
                &mut b,
                "related",
                related.as_deref().map(|x| json!(list_arg(x))),
            );
            put(&mut b, "owner", owner.map(Value::from));
            put(
                &mut b,
                "editors",
                editors.as_deref().map(|x| json!(list_arg(x))),
            );
            put(&mut b, "status", status.map(Value::from));
            put(&mut b, "pinned", (pin || unpin).then_some(json!(pin)));
            put(&mut b, "message", message.map(Value::from));
            put(&mut b, "if_rev", if_rev.map(Value::from));
            put(&mut b, "silent", silent.then_some(json!(true)));
            set(c, &doc, b).await
        }
        DocCmd::Supersede { doc, by, silent } => supersede(c, &doc, &by, silent).await,
        DocCmd::Verify { doc, for_ } => verify(c, &doc, for_).await,
        DocCmd::Split { doc, level, if_rev } => split(c, &doc, level, if_rev).await,
        DocCmd::Extract {
            doc,
            into,
            kind,
            if_rev,
        } => extract(c, &doc, None, &into, kind, if_rev).await,
        DocCmd::Merge { doc, into, if_rev } => merge(c, &doc, &into, if_rev).await,
        DocCmd::List {
            kind,
            status,
            team,
            owner,
            label,
            stale,
            pinned,
            limit,
            before,
        } => {
            list(
                c,
                &[
                    ("kind", kind),
                    ("status", status),
                    ("team", team),
                    ("owner", owner),
                    ("label", label),
                    ("stale", stale.then(|| "1".into())),
                    ("pinned", pinned.then(|| "1".into())),
                    ("limit", limit.map(|n| n.to_string())),
                    ("before", before.map(|n| n.to_string())),
                ],
            )
            .await
        }
        DocCmd::Search {
            terms,
            kind,
            status,
            team,
            label,
            owner,
            limit,
        } => {
            search(
                c,
                &[
                    ("q", Some(terms.join(" "))),
                    ("kind", kind),
                    ("status", status),
                    ("team", team),
                    ("label", label),
                    ("owner", owner),
                    ("limit", limit.map(|n| n.to_string())),
                ],
            )
            .await
        }
        DocCmd::Find {
            words,
            kind,
            status,
            limit,
        } => {
            find(
                c,
                &[
                    ("q", Some(words.join(" "))),
                    ("kind", kind),
                    ("status", status),
                    ("limit", limit.map(|n| n.to_string())),
                ],
            )
            .await
        }
        DocCmd::Index { team } => index(c, team).await,
        DocCmd::History { doc, all } => history(c, &doc, all).await,
        DocCmd::Diff { doc, from, to } => diff(c, &doc, from, to).await,
        DocCmd::Backlinks { doc } => backlinks(c, &doc).await,
        DocCmd::Check { team } => check(c, team).await,
        DocCmd::Comment { doc, body } => {
            let body = crate::read_body(body)?;
            comment(c, &doc, None, body).await
        }
        DocCmd::Kinds => kinds(c).await,
        DocCmd::Kind {
            cmd:
                KindCmd::Add {
                    name,
                    sections,
                    activate_by,
                    no_index,
                    cap,
                    description,
                    verify_days,
                    disable,
                    enable,
                },
        } => {
            let mut b = Map::new();
            put(&mut b, "name", Some(json!(name)));
            put(&mut b, "description", description.map(Value::from));
            put(
                &mut b,
                "required_sections",
                sections.as_deref().map(|x| json!(list_arg(x))),
            );
            put(&mut b, "activate_by", activate_by.map(Value::from));
            put(&mut b, "in_index", no_index.then_some(json!(false)));
            put(&mut b, "body_cap_bytes", cap.map(Value::from));
            put(&mut b, "verify_days", verify_days.map(Value::from));
            put(
                &mut b,
                "disabled",
                (disable || enable).then_some(json!(disable)),
            );
            let v = c
                .raw(Method::POST, "/docs/kinds", Some(&Value::Object(b)))
                .await?;
            let k: p::DocKind = crate::parse(&v)?;
            Ok((v, kind_line(&k)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refs() {
        assert_eq!(split_ref("d12").unwrap(), ("d12".into(), None));
        assert_eq!(
            split_ref("d12#rollback").unwrap(),
            ("d12".into(), Some("rollback".into()))
        );
        assert_eq!(
            split_ref("runbook.deploy#steps").unwrap(),
            ("runbook.deploy".into(), Some("steps".into()))
        );
        assert_eq!(split_ref("d12#").unwrap(), ("d12".into(), None));
        assert!(split_ref("#x").is_err());
        assert!(doc_only("d1#x").is_err());
        assert_eq!(path("a b"), "/docs/a%20b");
    }

    #[test]
    fn numbers_and_lists() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(52_000), "52,000");
        assert_eq!(thousands(1_234_567), "1,234,567");
        assert_eq!(list_arg("a, b,,c"), vec!["a", "b", "c"]);
        assert!(list_arg("none").is_empty());
        assert_eq!(or_null("none"), Value::Null);
    }

    #[test]
    fn client_keys() {
        let a = default_client_key("w1", "finding", "T", "");
        assert_eq!(a.len(), 32);
        assert_eq!(a, default_client_key("w1", "finding", "T", ""));
        assert_ne!(a, default_client_key("w2", "finding", "T", ""));
        assert_ne!(a, default_client_key("w1", "runbook", "T", ""));
        assert_ne!(a, default_client_key("w1", "finding", "T", "d"));
    }
}
