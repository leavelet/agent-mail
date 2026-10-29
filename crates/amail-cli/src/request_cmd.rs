//! `amail ask`, `answer`, `requests`, `request show`, `withdraw r12`,
//! `prompt` and `interrupt`: requests for the person (`/v1/requests…`)
//! and the two seat controls (`/v1/seats/{name}/prompt`, `/interrupt`).
//!
//! As in [`crate::doc_cmd`], each operation makes the API calls and returns
//! the server's JSON with its text rendering, so that the command line and
//! the MCP tools (`ask`, `requests`, `answer`, `prompt_seat`,
//! `interrupt_seat` in [`crate::mcp`]) print the same lines. Every value
//! that comes from the server goes through [`l`] or [`text::subject`];
//! bodies are printed behind `  > `.

use crate::Out;
use crate::client::{ApiError, Client, enc};
use crate::output::{l, short_time};
use amail_proto::{self as p, RequestState, text};
use anyhow::{Context, Result, anyhow, bail};
use reqwest::Method;
use serde_json::{Map, Value, json};

/// The longest wait of one `GET /requests/{num}/wait`, in seconds.
const WAIT_S: u64 = p::MAX_REQUEST_WAIT_S;

/// How long `ask --wait` keeps waiting past the deadline of a request
/// with a default, for the server's deadline pass to take it.
const DEFAULT_GRACE_S: i64 = 3_600;

#[derive(clap::Args)]
pub struct AskArgs {
    /// One line.
    pub title: String,
    /// The question or the detail (Markdown).
    #[arg(short = 'm', long, conflicts_with = "file")]
    pub body: Option<String>,
    /// Read the body from a file (`-`: stdin). Without -m or --file the
    /// body is read from stdin when stdin is not a terminal.
    #[arg(long)]
    pub file: Option<std::path::PathBuf>,
    /// A typed answer, `key:label` or `key:label:description` (2 to 6;
    /// keys of letters, digits, `_`, `-`, `.`, not digits only).
    #[arg(long = "option", value_name = "KEY:LABEL[:DESCRIPTION]")]
    pub options: Vec<String>,
    /// The answer may be free text (question and plan).
    #[arg(long)]
    pub free_text: bool,
    /// The option taken at the deadline if nobody answers.
    #[arg(long, value_name = "KEY")]
    pub default: Option<String>,
    /// now (minutes), soon (hours; the default) or later (a day).
    #[arg(long)]
    pub class: Option<String>,
    /// When the default is taken (without a default: when reminders
    /// start), from now: 30m, 2h, 1d. Default: from the class (15m, 4h,
    /// 24h).
    #[arg(long, value_name = "DURATION")]
    pub deadline: Option<String>,
    /// The task the request belongs to (tN): it shows blocked-on-human
    /// while the request is open.
    #[arg(long)]
    pub task: Option<String>,
    /// question (default), plan or merge.
    #[arg(long)]
    pub kind: Option<String>,
    /// Wait for the answer (or the deadline) and print it.
    #[arg(long)]
    pub wait: bool,
}

#[derive(clap::Args)]
pub struct ListArgs {
    /// open (default), all, or comma-separated states (answered, expired,
    /// withdrawn, superseded).
    #[arg(long)]
    pub state: Option<String>,
    /// question, plan, merge, attention.
    #[arg(long)]
    pub kind: Option<String>,
    /// The seat a request is about.
    #[arg(long)]
    pub seat: Option<String>,
    /// now, soon or later.
    #[arg(long)]
    pub class: Option<String>,
    /// A task (tN).
    #[arg(long)]
    pub task: Option<String>,
    /// At most this many (default 200, at most 500).
    #[arg(long)]
    pub limit: Option<u32>,
}

#[derive(clap::Subcommand)]
pub enum RequestCmd {
    /// A request: its fields, the options numbered as in its mail, the
    /// answer, the events and the threads.
    Show {
        /// r12 or 12.
        request: String,
    },
}

// ---------------------------------------------------------------------------
// Arguments

/// `r12` or `12`.
pub fn num(s: &str) -> Result<i64> {
    let t = s.trim();
    let t = t
        .strip_prefix('r')
        .or_else(|| t.strip_prefix('R'))
        .unwrap_or(t);
    match t.parse::<i64>() {
        Ok(n) if n > 0 => Ok(n),
        _ => bail!("{s:?} is not a request number (r12 or 12)"),
    }
}

/// Whether `s` names a request (`r12`) rather than a message id.
pub fn is_request_ref(s: &str) -> bool {
    let t = s.trim();
    (t.starts_with('r') || t.starts_with('R')) && num(t).is_ok()
}

/// `key:label[:description]`.
pub fn option(s: &str) -> Result<p::RequestOption> {
    let (key, rest) = s
        .split_once(':')
        .with_context(|| format!("option {s:?}: write it as key:label or key:label:description"))?;
    let (label, description) = match rest.split_once(':') {
        Some((a, b)) => (a, Some(b.trim().to_string()).filter(|d| !d.is_empty())),
        None => (rest, None),
    };
    let (key, label) = (key.trim(), label.trim());
    if key.is_empty() || label.is_empty() {
        bail!("option {s:?}: the key and the label must not be empty");
    }
    Ok(p::RequestOption {
        key: key.to_string(),
        label: label.to_string(),
        description,
    })
}

/// `30m`, `2h`, `1d` (or a number of seconds) as seconds.
pub fn deadline_s(s: &str) -> Result<u64> {
    if let Ok(n) = s.trim().parse::<u64>() {
        if n == 0 {
            bail!("the deadline must be in the future");
        }
        return Ok(n);
    }
    match amail_node::team::parse_duration(s).map_err(|e| anyhow!(e))? {
        Some(d) if d.as_secs() > 0 => Ok(d.as_secs()),
        _ => bail!("the deadline must be in the future (examples: 30m, 2h, 1d)"),
    }
}

// ---------------------------------------------------------------------------
// Rendering

/// `5m ago` or `in 2h` for a server time.
pub fn relative(ts: &str) -> String {
    let Ok(t) = ts.parse::<jiff::Timestamp>() else {
        return l(ts).into_owned();
    };
    let d = t.as_second() - jiff::Timestamp::now().as_second();
    let a = d.unsigned_abs();
    let n = match a {
        0..60 => format!("{a}s"),
        60..3_600 => format!("{}m", a / 60),
        3_600..86_400 => format!("{}h", a / 3_600),
        _ => format!("{}d", a / 86_400),
    };
    if d < 0 {
        format!("{n} ago")
    } else {
        format!("in {n}")
    }
}

/// `1. yes — Ship it` for an option key of the request (the key alone
/// when the request has no such option).
pub fn option_line(r: &p::Request, key: &str) -> String {
    match r.options.iter().position(|o| o.key == key) {
        Some(i) => format!("{}. {} — {}", i + 1, l(key), l(&r.options[i].label)),
        None => l(key).into_owned(),
    }
}

/// The answer as one line: the option or the text.
pub fn answer_value(r: &p::Request) -> String {
    match &r.answer {
        Some(a) => match (&a.option, &a.text) {
            (Some(k), Some(t)) => format!("{} ({})", option_line(r, k), l(t)),
            (Some(k), None) => option_line(r, k),
            (None, Some(t)) => l(t).into_owned(),
            (None, None) => String::new(),
        },
        None => String::new(),
    }
}

/// One request on one line: number, state, class, kind, seat, age, title,
/// and the task, the deadline or the answer.
pub fn request_line(r: &p::Request) -> String {
    let mut s = format!(
        "{:<5} {:<9} {:<5} {:<9} {:<8} {:<8} {}",
        format!("r{}", r.num),
        r.state.as_str(),
        r.class.as_str(),
        r.kind.as_str(),
        l(&r.seat),
        relative(&r.created_at),
        text::subject(&r.title)
    );
    if let Some(t) = r.task {
        s.push_str(&format!("  · t{t}"));
    }
    if r.state == RequestState::Open {
        if let Some(d) = &r.deadline {
            match &r.default {
                Some(k) => s.push_str(&format!("  · default {} {}", l(k), deadline_when(d))),
                None => s.push_str(&format!("  · no default, reminders {}", deadline_when(d))),
            }
        }
    } else if r.answer.is_some() {
        s.push_str(&format!("  · {}", answer_value(r)));
    }
    s
}

/// `at 2026-10-29 14:00Z (in 3h)`.
fn deadline_when(d: &str) -> String {
    format!("at {} ({})", short_time(d), relative(d))
}

/// What `ask` prints: `r12  question  soon  <title>  (deadline …)`.
pub fn created_line(r: &p::Request) -> String {
    let mut s = format!(
        "r{}  {}  {}  {}",
        r.num,
        r.kind.as_str(),
        r.class.as_str(),
        text::subject(&r.title)
    );
    if let Some(d) = &r.deadline {
        match &r.default {
            Some(k) => s.push_str(&format!(
                "  (deadline {}, then the default {})",
                short_time(d),
                l(k)
            )),
            None => s.push_str(&format!(
                "  (deadline {}; no default: reminders after it)",
                short_time(d)
            )),
        }
    }
    if let Some(t) = r.task {
        s.push_str(&format!("  · t{t} is blocked-on-human"));
    }
    s
}

/// What `ask` prints without `--wait`.
pub fn created_text(d: &p::RequestDetail) -> String {
    let r = &d.request;
    format!(
        "{}\nThe answer comes as mail in the request's thread (Re: [r{n}] …). Show it: amail request show r{n}",
        created_line(r),
        n = r.num
    )
}

/// How a request that is no longer open ended, or that it is still open.
pub fn outcome_text(r: &p::Request) -> String {
    let n = r.num;
    let note = r
        .answer
        .as_ref()
        .and_then(|a| a.note.as_deref())
        .map(|x| format!("\nNote: {}", l(x)))
        .unwrap_or_default();
    match r.state {
        RequestState::Answered => {
            let by = r.answer.as_ref().map(|a| l(&a.by).into_owned());
            let mut s = format!(
                "r{n} answered by {}: {}",
                by.unwrap_or_default(),
                answer_value(r)
            );
            if let Some(t) = r
                .answer
                .as_ref()
                .and_then(|a| a.text.as_deref())
                .filter(|t| t.contains('\n'))
            {
                s = format!(
                    "r{n} answered by {}:\n{}",
                    l(&r.answer.as_ref().unwrap().by),
                    text::quote(t).trim_end()
                );
            }
            s + &note
        }
        RequestState::Expired => format!(
            "r{n} expired: nobody answered by the deadline; it took its default {}{note}",
            answer_value(r)
        ),
        RequestState::Withdrawn => format!("r{n} was withdrawn"),
        RequestState::Superseded => format!("r{n} was superseded by a newer request"),
        RequestState::Open => format!("r{n} is still open"),
    }
}

fn event_line(e: &p::RequestEvent) -> String {
    let mut s = format!(
        "{}  {}  {}",
        short_time(&e.at),
        e.actor
            .as_deref()
            .map(|a| l(a).into_owned())
            .unwrap_or("server".into()),
        e.kind
    );
    if let Some(n) = &e.note {
        s.push_str(&format!(" ({})", l(n)));
    }
    if let Some(m) = e.message_id {
        s.push_str(&format!(" #{m}"));
    }
    s
}

/// The options numbered as in the request's mail.
fn options_lines(r: &p::Request, out: &mut Vec<String>) {
    if !r.options.is_empty() {
        out.push("Options:".into());
        for (i, o) in r.options.iter().enumerate() {
            let mut s = format!("  {}. {} — {}", i + 1, l(&o.key), l(&o.label));
            if r.default.as_deref() == Some(o.key.as_str()) {
                s.push_str("  (default)");
            }
            out.push(s);
            if let Some(d) = o.description.as_deref().filter(|d| !d.trim().is_empty()) {
                out.extend(d.lines().map(|x| format!("     {}", l(x))));
            }
        }
    }
    if r.free_text {
        out.push(if r.options.is_empty() {
            "Answer with free text.".into()
        } else {
            "Or answer with free text.".into()
        });
    }
}

/// `request show`: the fields, the options, the answer, the threads, the
/// events, the body behind `  > `, and how to answer an open request.
pub fn detail_text(d: &p::RequestDetail) -> String {
    let r = &d.request;
    let mut out = vec![format!("r{} {}", r.num, l(&r.title))];
    out.push(format!(
        "Kind: {} · Class: {} · State: {} · Rev: {}",
        r.kind, r.class, r.state, r.rev
    ));
    let mut who = format!("Seat: {}", l(&r.seat));
    if let Some(a) = &r.asker
        && a != &r.seat
    {
        who.push_str(&format!(" (asked by {})", l(a)));
    }
    if let Some(t) = r.task {
        who.push_str(&format!(" · Task: t{t}"));
    }
    who.push_str(&format!(" · Source: {}", r.source));
    out.push(who);
    if let Some(a) = &r.attention {
        out.push(format!("Attention: {}", l(a)));
    }
    out.push(format!(
        "Created: {} ({})",
        short_time(&r.created_at),
        relative(&r.created_at)
    ));
    if let Some(dl) = &r.deadline {
        let what = if r.default.is_some() {
            "the default is taken"
        } else {
            "reminders start"
        };
        out.push(format!(
            "Deadline: {} ({}; {what})",
            short_time(dl),
            relative(dl)
        ));
    }
    match &r.default {
        Some(k) => out.push(format!("Default: {}", option_line(r, k))),
        None if r.kind != p::RequestKind::Attention => {
            out.push("Default: none (the request stays open until it is answered)".into())
        }
        None => {}
    }
    options_lines(r, &mut out);
    if let Some(a) = &r.answer {
        out.push(format!(
            "Answer: {} · by {} at {}",
            answer_value(r),
            l(&a.by),
            short_time(&a.at)
        ));
        if let Some(n) = &a.note {
            out.push(format!("Answer note: {}", l(n)));
        }
    }
    if let Some(t) = &d.terminal_url {
        out.push(format!("Terminal: {}", l(t)));
    }
    if let Some(t) = r.thread {
        out.push(format!("Thread: {t} (amail thread {t})"));
    }
    if !d.threads.is_empty() {
        let v: Vec<String> = d.threads.iter().map(|t| t.to_string()).collect();
        out.push(format!("Other threads that name it: {}", v.join(", ")));
    }
    if !d.events.is_empty() {
        out.push("Events:".into());
        out.extend(d.events.iter().map(|e| format!("  {}", event_line(e))));
    }
    if !r.body.trim().is_empty() {
        out.push(String::new());
        out.push(text::quote(&r.body).trim_end().to_string());
    }
    if r.state == RequestState::Open {
        out.push(String::new());
        out.push(format!(
            "Answer it (user): amail answer r{} <option number, key or label{}>",
            r.num,
            if r.free_text { ", or text" } else { "" }
        ));
    }
    out.join("\n")
}

/// The text of a 409 `answered`, `stale` or `not_open` answer: the request
/// as it is now.
pub fn conflict_text(code: &str, r: &p::Request) -> String {
    let what = match code {
        "stale" => format!(
            "r{} changed since your revision; it is now at rev {}. Nothing was changed.",
            r.num, r.rev
        ),
        "answered" => format!("r{} was answered before; nothing was changed.", r.num),
        _ => format!("r{} is not open; nothing was changed.", r.num),
    };
    format!("{what}\n{}", outcome_text(r))
}

fn list_text(l: &p::RequestList, empty: &str) -> String {
    let mut lines: Vec<String> = l.requests.iter().map(request_line).collect();
    if lines.is_empty() {
        lines.push(empty.to_string());
    }
    if l.truncated {
        lines.push("(more: narrow the list or raise --limit)".into());
    }
    lines.join("\n")
}

// ---------------------------------------------------------------------------
// Operations, shared with the MCP tools: each answers the server's JSON and
// its text.

pub type Answer = (Value, String);

/// `POST /requests`.
pub async fn ask(c: &Client, body: Map<String, Value>) -> Result<Answer> {
    let v = c
        .raw(Method::POST, "/requests", Some(&Value::Object(body)))
        .await?;
    let d: p::RequestDetail = crate::parse(&v)?;
    Ok((v, created_text(&d)))
}

/// `GET /requests` with the given filters.
pub async fn list(c: &Client, pairs: &[(&str, Option<String>)]) -> Result<Answer> {
    let v = c
        .raw(
            Method::GET,
            &format!("/requests{}", crate::doc_cmd::query(pairs)),
            None,
        )
        .await?;
    let r: p::RequestList = crate::parse(&v)?;
    let open_only = pairs
        .iter()
        .any(|(k, v)| *k == "state" && v.as_deref().is_none_or(|s| s == "open"));
    let empty = if open_only {
        "no open requests"
    } else {
        "no requests"
    };
    Ok((v, list_text(&r, empty)))
}

/// `GET /requests/{num}`.
pub async fn show(c: &Client, n: i64) -> Result<Answer> {
    let v = c.raw(Method::GET, &format!("/requests/{n}"), None).await?;
    let d: p::RequestDetail = crate::parse(&v)?;
    Ok((v, detail_text(&d)))
}

/// `POST /requests/{num}/answer`.
pub async fn answer(c: &Client, n: i64, body: Map<String, Value>) -> Result<Answer> {
    let v = c
        .raw(
            Method::POST,
            &format!("/requests/{n}/answer"),
            Some(&Value::Object(body)),
        )
        .await?;
    let d: p::RequestDetail = crate::parse(&v)?;
    let mut t = outcome_text(&d.request);
    if let Some(u) = &d.terminal_url {
        t.push_str(&format!("\nThe seat's terminal: {}", l(u)));
    }
    Ok((v, t))
}

/// `POST /requests/{num}/withdraw`.
pub async fn withdraw(c: &Client, n: i64, note: Option<String>) -> Result<Answer> {
    let body = match note {
        Some(x) => json!({ "note": x }),
        None => json!({}),
    };
    let v = c
        .raw(
            Method::POST,
            &format!("/requests/{n}/withdraw"),
            Some(&body),
        )
        .await?;
    let d: p::RequestDetail = crate::parse(&v)?;
    Ok((
        v,
        format!("withdrew r{}: {}", n, text::subject(&d.request.title)),
    ))
}

/// Wait until request `n` is no longer open: one long poll after another.
/// Stops at the deadline of a request without a default (`done: false`);
/// a request with a default is waited for until the server takes the
/// default (at most an hour past the deadline).
pub async fn wait(c: &Client, n: i64) -> Result<Answer> {
    loop {
        let v = c
            .raw(
                Method::GET,
                &format!("/requests/{n}/wait?timeout_s={WAIT_S}"),
                None,
            )
            .await?;
        let w: p::RequestWait = crate::parse(&v)?;
        if w.done {
            return Ok((v, outcome_text(&w.request)));
        }
        let late = |grace: i64| {
            w.request
                .deadline
                .as_deref()
                .and_then(|d| d.parse::<jiff::Timestamp>().ok())
                .is_some_and(|d| jiff::Timestamp::now().as_second() >= d.as_second() + grace)
        };
        let past = if w.request.default.is_some() {
            late(DEFAULT_GRACE_S)
        } else {
            late(0)
        };
        if past {
            let t = format!(
                "r{n} has no answer by its deadline and stays open (amail request show r{n})"
            );
            return Ok((v, t));
        }
    }
}

/// `POST /seats/{name}/prompt`.
pub async fn prompt(c: &Client, seat: &str, text_: &str) -> Result<Answer> {
    let r = c
        .raw(
            Method::POST,
            &format!("/seats/{}/prompt", enc(seat)),
            Some(&json!({ "text": text_ })),
        )
        .await;
    let v = r.map_err(|e| control_err(e, seat))?;
    let s: p::SeatControlResponse = crate::parse(&v)?;
    Ok((
        v,
        format!(
            "prompt sent to {} (through {} node{})",
            l(&s.seat),
            s.nodes,
            if s.nodes == 1 { "" } else { "s" }
        ),
    ))
}

/// `POST /seats/{name}/interrupt`.
pub async fn interrupt(c: &Client, seat: &str) -> Result<Answer> {
    let r = c
        .raw(
            Method::POST,
            &format!("/seats/{}/interrupt", enc(seat)),
            None,
        )
        .await;
    let v = r.map_err(|e| control_err(e, seat))?;
    let s: p::SeatControlResponse = crate::parse(&v)?;
    Ok((
        v,
        format!(
            "interrupt sent to {} (through {} node{})",
            l(&s.seat),
            s.nodes,
            if s.nodes == 1 { "" } else { "s" }
        ),
    ))
}

/// The 409s of the seat controls in plain words; the API error stays the
/// cause (for `--json` and the MCP tools' codes).
fn control_err(e: anyhow::Error, seat: &str) -> anyhow::Error {
    let mut a = match e.downcast::<ApiError>() {
        Ok(a) => a,
        Err(e) => return e,
    };
    let plain = match a.body.error.code.as_str() {
        "no_structured_interrupt" => Some(format!(
            "{seat}: Claude Code sessions have no structured interrupt; use the terminal \
             (amail attach {seat}, or the terminal link on the seat's web page)",
            seat = l(seat)
        )),
        "no_node" => Some(format!("{}: no node is connected for this seat", l(seat))),
        _ => None,
    };
    if let Some(m) = plain {
        a.body.error.message = m;
    }
    a.into()
}

/// The request a 409 carries, if any.
pub fn conflict_request(e: &anyhow::Error) -> Option<(String, p::Request)> {
    let api = e.downcast_ref::<ApiError>()?;
    let code = api.body.error.code.as_str();
    if !matches!(code, "answered" | "stale" | "not_open") {
        return None;
    }
    let r = serde_json::from_value(api.raw.get("request")?.clone()).ok()?;
    Some((code.to_string(), r))
}

// ---------------------------------------------------------------------------
// The command line

fn put(b: &mut Map<String, Value>, k: &str, v: Option<Value>) {
    if let Some(v) = v {
        b.insert(k.into(), v);
    }
}

/// The body of `POST /requests` from the command's arguments.
pub fn ask_body(a: &AskArgs, body: Option<String>, source: &str) -> Result<Map<String, Value>> {
    let options = a
        .options
        .iter()
        .map(|o| option(o))
        .collect::<Result<Vec<_>>>()?;
    let mut b = Map::new();
    put(&mut b, "title", Some(json!(a.title)));
    put(&mut b, "kind", a.kind.clone().map(Value::from));
    put(&mut b, "task", a.task.clone().map(Value::from));
    put(
        &mut b,
        "body",
        body.filter(|x| !x.trim().is_empty()).map(Value::from),
    );
    put(
        &mut b,
        "options",
        (!options.is_empty()).then(|| json!(options)),
    );
    put(&mut b, "free_text", a.free_text.then_some(json!(true)));
    put(&mut b, "default", a.default.clone().map(Value::from));
    put(&mut b, "class", a.class.clone().map(Value::from));
    put(
        &mut b,
        "deadline_s",
        a.deadline
            .as_deref()
            .map(deadline_s)
            .transpose()?
            .map(Value::from),
    );
    put(&mut b, "source", Some(json!(source)));
    Ok(b)
}

/// The body of `ask`: `-m`, `--file` (`-`: stdin), or stdin when it is not
/// a terminal.
fn ask_text(a: &AskArgs) -> Result<Option<String>> {
    use std::io::IsTerminal;
    match (&a.body, &a.file) {
        (Some(b), _) => Ok(Some(b.clone())),
        (None, Some(f)) if f.as_os_str() == "-" => crate::read_body(None).map(Some),
        (None, Some(f)) => std::fs::read_to_string(f)
            .map(Some)
            .with_context(|| format!("reading {}", f.display())),
        (None, None) if !std::io::stdin().is_terminal() => crate::read_body(None).map(Some),
        (None, None) => Ok(None),
    }
}

/// Print an error's request (a 409 `answered`, `stale`, `not_open`) before
/// the error.
fn show_conflict(out: &Out, e: &anyhow::Error) -> Result<()> {
    if let Some((code, r)) = conflict_request(e) {
        let raw = e
            .downcast_ref::<ApiError>()
            .map(|a| a.raw.clone())
            .unwrap_or_default();
        out.show(&raw, || Ok(conflict_text(&code, &r)))?;
    }
    Ok(())
}

pub async fn run_ask(c: &Client, out: &Out, a: AskArgs) -> Result<()> {
    let body = ask_text(&a)?;
    let b = ask_body(&a, body, "cli")?;
    let (v, t) = ask(c, b).await?;
    if !a.wait {
        return out.show(&v, || Ok(t));
    }
    let d: p::RequestDetail = crate::parse(&v)?;
    // The number first, so that a person watching knows what to answer.
    if !out.json {
        out.show(&v, || Ok(created_line(&d.request)))?;
    }
    let (w, t) = wait(c, d.request.num).await?;
    out.show(&w, || Ok(t))?;
    if w.get("done") == Some(&Value::Bool(false)) {
        bail!("r{} has no answer", d.request.num);
    }
    Ok(())
}

pub async fn run_answer(
    c: &Client,
    out: &Out,
    request: &str,
    answer_: &str,
    note: Option<String>,
    if_rev: Option<i64>,
) -> Result<()> {
    let n = num(request)?;
    let mut b = Map::new();
    put(&mut b, "text", Some(json!(answer_)));
    put(&mut b, "note", note.map(Value::from));
    put(&mut b, "if_rev", if_rev.map(Value::from));
    match answer(c, n, b).await {
        Ok((v, t)) => out.show(&v, || Ok(t)),
        Err(e) => {
            show_conflict(out, &e)?;
            Err(e)
        }
    }
}

pub async fn run_withdraw(
    c: &Client,
    out: &Out,
    request: &str,
    note: Option<String>,
) -> Result<()> {
    let n = num(request)?;
    match withdraw(c, n, note).await {
        Ok((v, t)) => out.show(&v, || Ok(t)),
        Err(e) => {
            show_conflict(out, &e)?;
            Err(e)
        }
    }
}

pub async fn run_list(c: &Client, out: &Out, a: ListArgs) -> Result<()> {
    let (v, t) = list(
        c,
        &[
            ("state", a.state),
            ("kind", a.kind),
            ("seat", a.seat),
            ("class", a.class),
            ("task", a.task),
            ("limit", a.limit.map(|n| n.to_string())),
        ],
    )
    .await?;
    out.show(&v, || Ok(t))
}

pub async fn run(c: &Client, out: &Out, cmd: RequestCmd) -> Result<()> {
    match cmd {
        RequestCmd::Show { request } => {
            let (v, t) = show(c, num(&request)?).await?;
            out.show(&v, || Ok(t))
        }
    }
}

pub async fn run_prompt(c: &Client, out: &Out, seat: &str, body: Option<String>) -> Result<()> {
    let t = crate::read_body(body)?;
    if t.trim().is_empty() {
        bail!("the prompt is empty: give it with -m or on stdin");
    }
    let (v, t) = prompt(c, seat, &t).await?;
    out.show(&v, || Ok(t))
}

pub async fn run_interrupt(c: &Client, out: &Out, seat: &str) -> Result<()> {
    let (v, t) = interrupt(c, seat).await?;
    out.show(&v, || Ok(t))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        assert_eq!(num("r12").unwrap(), 12);
        assert_eq!(num("12").unwrap(), 12);
        assert_eq!(num(" R3 ").unwrap(), 3);
        assert!(num("r0").is_err());
        assert!(num("t12").is_err());
        assert!(is_request_ref("r12"));
        assert!(!is_request_ref("12"));
        assert!(!is_request_ref("rx"));
    }

    #[test]
    fn options() {
        let o = option("yes:Ship it").unwrap();
        assert_eq!((o.key.as_str(), o.label.as_str()), ("yes", "Ship it"));
        assert!(o.description.is_none());
        let o = option("no:Hold it:until Monday").unwrap();
        assert_eq!(o.description.as_deref(), Some("until Monday"));
        assert!(option("yes").is_err());
        assert!(option(":x").is_err());
        assert!(option("k:").is_err());
    }

    #[test]
    fn deadlines() {
        assert_eq!(deadline_s("2h").unwrap(), 7_200);
        assert_eq!(deadline_s("1h30m").unwrap(), 5_400);
        assert_eq!(deadline_s("90").unwrap(), 90);
        assert!(deadline_s("0").is_err());
        assert!(deadline_s("off").is_err());
        assert!(deadline_s("soon").is_err());
    }
}
