//! `amail handover …` and `amail worklog`: a seat's handover note and its
//! worklog (`/v1/seats/{name}/handover`, `/v1/seats/{name}/worklog`).
//!
//! As in [`crate::doc_cmd`], each operation answers the server's JSON and
//! its text, shared with the MCP tools `handover_show`, `handover_write`
//! and `worklog`.

use crate::Out;
use crate::client::{Client, enc};
use crate::doc_cmd::{Answer, query, thousands};
use crate::output::{l, short_time};
use amail_proto::{self as p, text};
use anyhow::{Context, Result, bail};
use reqwest::Method;
use serde_json::{Map, Value, json};

#[derive(clap::Subcommand)]
pub enum HandoverCmd {
    /// A seat's handover note (default: yours), its revisions, and who
    /// read it.
    Show {
        seat: Option<String>,
        /// An earlier revision.
        #[arg(long)]
        rev: Option<i64>,
    },
    /// Write your handover note: the seven fields as `Field:` lines or
    /// Markdown headings (see `amail handover template`), from -m, a file
    /// (`-`: stdin) or stdin.
    Write {
        /// A file with the note.
        file: Option<std::path::PathBuf>,
        #[arg(short = 'm', long, conflicts_with = "file")]
        text: Option<String>,
        /// Only the seat, its lead and the user may read the note.
        #[arg(long, conflicts_with = "public")]
        private: bool,
        /// Every seat of the org may read the note again.
        #[arg(long)]
        public: bool,
        /// The session that writes (Pointers are filled in from its
        /// worklog entries when left empty).
        #[arg(long)]
        session: Option<String>,
        /// User tokens: the seat whose note this is.
        #[arg(long)]
        seat: Option<String>,
    },
    /// Print the note's template: the seven fields in order.
    Template,
    /// Ask a seat for its note (its lead or the user); the seat gets mail.
    Request { seat: String },
}

#[derive(clap::Args)]
pub struct WorklogArgs {
    /// The seat (default: yours).
    pub seat: Option<String>,
    /// Entries since a duration ago (30m, 2h, 7d) or an RFC 3339 time.
    #[arg(long)]
    pub since: Option<String>,
    /// Entries of one task (t12).
    #[arg(long, value_name = "TASK")]
    pub task: Option<String>,
    /// Entries of one kind (mail_sent, task, tool, commit, ...).
    #[arg(long)]
    pub kind: Option<String>,
    #[arg(long)]
    pub limit: Option<u32>,
    /// Continue an earlier listing (its `next_before`).
    #[arg(long)]
    pub before: Option<i64>,
    /// Counts per kind, the tasks touched and the last ten entries.
    #[arg(long)]
    pub summary: bool,
}

/// The seat named, or the caller's.
async fn seat_or_me(c: &Client, seat: Option<String>) -> Result<String> {
    if let Some(s) = seat.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
        return Ok(s);
    }
    let me: p::WhoAmI = c.get("/whoami").await?;
    me.seat
        .context("this token has no seat: name the seat (amail handover show <seat>)")
}

// ---------------------------------------------------------------------------
// Rendering

fn source_text(n: &p::HandoverNote) -> String {
    let mut s = n.source.to_string();
    if n.draft {
        s.push_str(", draft");
    }
    if n.private {
        s.push_str(", private");
    }
    s
}

fn revision_line(r: &p::HandoverRevision) -> String {
    let mut s = format!(
        "rev {:<3} {}  {}  {} chars",
        r.rev,
        short_time(&r.written_at),
        r.source,
        thousands(r.chars as i64)
    );
    if r.private {
        s.push_str("  private");
    }
    if let Some(x) = &r.written_by {
        s.push_str(&format!("  by {}", l(x)));
    }
    if let Some(x) = &r.session {
        s.push_str(&format!("  session {}", l(x)));
    }
    if let Some(x) = &r.read_by_session {
        s.push_str(&format!("  read by session {}", l(x)));
    }
    s
}

/// A seat's note: a header, who read it, the fields behind `  > `, and the
/// revisions.
pub fn show_text(r: &p::HandoverResponse) -> String {
    let mut out = Vec::new();
    match &r.note {
        None => out.push(format!("{} has no handover note", l(&r.seat))),
        Some(n) => {
            let mut h = format!(
                "Handover note of {}, rev {} ({}), written {}",
                l(&n.seat),
                n.rev,
                source_text(n),
                short_time(&n.written_at)
            );
            if let Some(w) = &n.written_by {
                h.push_str(&format!(" by {}", l(w)));
            }
            if let Some(s) = &n.session {
                h.push_str(&format!(" in session {}", l(s)));
            }
            if n.rev != r.current_rev {
                h.push_str(&format!(" (the current one is rev {})", r.current_rev));
            }
            out.push(h);
            match (&n.read_by_session, &n.read_at) {
                (Some(s), Some(at)) => {
                    out.push(format!("Read by: session {} at {}", l(s), short_time(at)))
                }
                (Some(s), None) => out.push(format!("Read by: session {}", l(s))),
                _ => out.push("Read by: no session yet".into()),
            }
            if n.pointers_filled {
                out.push("(Pointers were filled in by the server from the worklog)".into());
            }
        }
    }
    if let Some(at) = &r.requested_at {
        let by = r
            .requested_by
            .as_deref()
            .map(|b| format!(" by {}", l(b)))
            .unwrap_or_default();
        out.push(format!(
            "Requested{by} at {} (not answered yet: amail handover write)",
            short_time(at)
        ));
    }
    if let Some(n) = &r.note {
        out.push(String::new());
        out.push(text::quote(&n.text).trim_end().to_string());
    }
    if r.revisions.len() > 1 {
        out.push(String::new());
        out.push("Revisions:".into());
        out.extend(
            r.revisions
                .iter()
                .map(|x| format!("  {}", revision_line(x))),
        );
    }
    out.join("\n")
}

/// The answer of a write.
pub fn written_text(r: &p::HandoverResponse) -> String {
    match &r.note {
        Some(n) => {
            let mut s = format!(
                "handover note of {}: rev {} written ({} characters{})",
                l(&n.seat),
                n.rev,
                thousands(n.chars as i64),
                if n.private { ", private" } else { "" }
            );
            if n.pointers_filled {
                s.push_str("; Pointers filled in from the worklog:\n");
                s.push_str(&text::quote(&n.fields.pointers));
            }
            s
        }
        None => format!("handover note of {} written", l(&r.seat)),
    }
}

/// The lines for a 422 `note_fields_missing`: the missing labels and where
/// the template is.
pub fn missing_text(raw: &Value) -> Option<String> {
    let missing: Vec<String> = raw
        .get("missing")?
        .as_array()?
        .iter()
        .filter_map(|v| v.as_str().map(|s| l(s).into_owned()))
        .collect();
    Some(format!(
        "The note lacks: {}.\nThe fields, in order: {}. Each is a `Field:` line or a Markdown heading; \
         Pointers may be left empty. The template: amail handover template",
        missing.join(", "),
        p::HANDOVER_FIELDS.join(", ")
    ))
}

/// `2026-10-27 10:00Z  mail_sent  m12 "subject" to b  · t12`.
pub fn entry_line(e: &p::WorklogEntry) -> String {
    let mut s = format!("{}  {:<10} {}", short_time(&e.at), l(&e.kind), l(&e.text));
    if let Some(t) = e.task {
        s.push_str(&format!("  · t{t}"));
    }
    if let Some(d) = e.doc {
        s.push_str(&format!("  · d{d}"));
    }
    s
}

pub fn page_text(r: &p::WorklogPage) -> String {
    let mut lines: Vec<String> = r.entries.iter().map(entry_line).collect();
    if lines.is_empty() {
        lines.push(format!("no worklog entries of {}", l(&r.seat)));
    }
    if let Some(b) = r.next_before {
        lines.push(format!(
            "(older: amail worklog {} --before {b} ...)",
            l(&r.seat)
        ));
    }
    lines.join("\n")
}

pub fn summary_text(r: &p::WorklogSummary) -> String {
    let mut out = vec![format!("worklog of {}: {} entries", l(&r.seat), r.total)];
    if !r.kinds.is_empty() {
        let v: Vec<String> = r
            .kinds
            .iter()
            .map(|k| format!("{} {}", l(&k.kind), k.count))
            .collect();
        out.push(format!("kinds: {}", v.join(", ")));
    }
    if !r.tasks.is_empty() {
        let v: Vec<String> = r
            .tasks
            .iter()
            .map(|t| format!("t{} ({}, last {})", t.task, t.count, short_time(&t.last_at)))
            .collect();
        out.push(format!("tasks: {}", v.join(", ")));
    }
    if !r.last.is_empty() {
        out.push(format!("last {}:", r.last.len()));
        out.extend(r.last.iter().map(|e| format!("  {}", entry_line(e))));
    }
    out.join("\n")
}

// ---------------------------------------------------------------------------
// Operations

fn note_path(seat: &str) -> String {
    format!("/seats/{}/handover", enc(seat))
}

pub async fn show(c: &Client, seat: Option<String>, rev: Option<i64>) -> Result<Answer> {
    let seat = seat_or_me(c, seat).await?;
    let q = query(&[("rev", rev.map(|r| r.to_string()))]);
    let v = c
        .raw(Method::GET, &format!("{}{q}", note_path(&seat)), None)
        .await?;
    let r: p::HandoverResponse = crate::parse(&v)?;
    Ok((v, show_text(&r)))
}

/// `PUT /seats/{seat}/handover`; `body` holds `text` or `fields`, and
/// optionally `private` and `session`.
pub async fn write(c: &Client, seat: Option<String>, body: Map<String, Value>) -> Result<Answer> {
    let seat = seat_or_me(c, seat).await?;
    let v = c
        .raw(Method::PUT, &note_path(&seat), Some(&Value::Object(body)))
        .await?;
    let r: p::HandoverResponse = crate::parse(&v)?;
    Ok((v, written_text(&r)))
}

pub async fn request(c: &Client, seat: &str) -> Result<Answer> {
    let v = c
        .raw(
            Method::POST,
            &format!("{}/request", note_path(seat.trim())),
            None,
        )
        .await?;
    let r: p::HandoverRequested = crate::parse(&v)?;
    let mail = r
        .message_id
        .map(|m| format!(" (mail #{m})"))
        .unwrap_or_default();
    Ok((
        v,
        format!(
            "asked {} for its handover note at {}{mail}",
            l(&r.seat),
            short_time(&r.requested_at)
        ),
    ))
}

/// `GET /seats/{seat}/worklog`, with `summary` the counts.
pub async fn worklog(
    c: &Client,
    seat: Option<String>,
    q: &[(&str, Option<String>)],
    summary: bool,
) -> Result<Answer> {
    let seat = seat_or_me(c, seat).await?;
    let mut pairs = q.to_vec();
    if summary {
        pairs.push(("summary", Some("1".into())));
    }
    let v = c
        .raw(
            Method::GET,
            &format!("/seats/{}/worklog{}", enc(&seat), query(&pairs)),
            None,
        )
        .await?;
    let t = if summary {
        summary_text(&crate::parse(&v)?)
    } else {
        page_text(&crate::parse(&v)?)
    };
    Ok((v, t))
}

// ---------------------------------------------------------------------------
// The command line

/// Print an error answer's extra lines (a note missing fields) before the
/// error itself.
fn explain(out: &Out, e: &anyhow::Error) -> Result<()> {
    if let Some(api) = e.downcast_ref::<crate::client::ApiError>()
        && api.body.error.code == "note_fields_missing"
        && let Some(t) = missing_text(&api.raw)
    {
        out.show(&api.raw, || Ok(t))?;
    }
    Ok(())
}

/// `amail handover template` (needs no server).
pub fn template(out: &Out) -> Result<()> {
    let v = json!({ "template": p::HANDOVER_TEMPLATE, "fields": p::HANDOVER_FIELDS });
    out.show(&v, || Ok(p::HANDOVER_TEMPLATE.trim_end().to_string()))
}

pub async fn run(c: &Client, out: &Out, cmd: HandoverCmd) -> Result<()> {
    let r = match cmd {
        HandoverCmd::Show { seat, rev } => show(c, seat, rev).await,
        HandoverCmd::Template => return template(out),
        HandoverCmd::Request { seat } => request(c, &seat).await,
        HandoverCmd::Write {
            file,
            text,
            private,
            public,
            session,
            seat,
        } => {
            let note = match (text, file) {
                (Some(t), _) => t,
                (None, Some(f)) if f.as_os_str() == "-" => crate::read_body(None)?,
                (None, Some(f)) => std::fs::read_to_string(&f)
                    .with_context(|| format!("reading {}", f.display()))?,
                (None, None) => crate::read_body(None)?,
            };
            if note.trim().is_empty() {
                bail!("the note is empty (see amail handover template)");
            }
            let mut b = Map::new();
            b.insert("text".into(), json!(note));
            if private || public {
                b.insert("private".into(), json!(private));
            }
            if let Some(s) = session {
                b.insert("session".into(), json!(s));
            }
            write(c, seat, b).await
        }
    };
    match r {
        Ok((v, t)) => out.show(&v, || Ok(t)),
        Err(e) => {
            explain(out, &e)?;
            Err(e)
        }
    }
}

pub async fn run_worklog(c: &Client, out: &Out, a: WorklogArgs) -> Result<()> {
    let (v, t) = worklog(
        c,
        a.seat,
        &[
            ("since", a.since),
            ("task", a.task),
            ("kind", a.kind),
            ("limit", a.limit.map(|n| n.to_string())),
            ("before", a.before.map(|n| n.to_string())),
        ],
        a.summary,
    )
    .await?;
    out.show(&v, || Ok(t))
}
