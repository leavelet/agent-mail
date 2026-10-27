//! The Markdown side of documents: sections with anchors, links out, the
//! token estimate, link rewriting, diffs and the similarity of titles.
//! Pure functions; the store calls them when it writes a revision.

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

/// One section of a body: a heading and its text, or (level 0, anchor
/// `""`) the text before the first heading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub level: i64,
    pub anchor: String,
    pub heading: String,
    /// `Deploy > Rollback`.
    pub path: String,
    /// Byte range with the subsections (up to the next heading of the same
    /// or a higher level).
    pub start: usize,
    pub end: usize,
    /// End of the section's own text (the next heading of any level).
    pub own_end: usize,
    /// End of the heading itself (its line).
    pub heading_end: usize,
    /// Token estimate of `start..end`.
    pub tokens: i64,
}

/// What a link points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LinkKind {
    Doc,
    Task,
    Message,
    Thread,
    Seat,
    Path,
}

impl LinkKind {
    pub fn as_str(self) -> &'static str {
        match self {
            LinkKind::Doc => "doc",
            LinkKind::Task => "task",
            LinkKind::Message => "message",
            LinkKind::Thread => "thread",
            LinkKind::Seat => "seat",
            LinkKind::Path => "path",
        }
    }
}

/// A link found in a body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub kind: LinkKind,
    /// The number (doc, task, message, thread) or the name or path.
    pub id: String,
    pub num: Option<i64>,
    pub anchor: Option<String>,
    /// A repository path's `:line`.
    pub line: Option<i64>,
    /// Byte offset in the body.
    pub offset: usize,
}

impl Link {
    /// The target as written: `d12#rollback`, `t3`, `#45`, `thread 7`,
    /// `@w1`, `src/a.rs:10`.
    pub fn shown(&self) -> String {
        shown_target(self.kind, &self.id, self.anchor.as_deref(), self.line)
    }
}

/// See [`Link::shown`].
pub fn shown_target(kind: LinkKind, id: &str, anchor: Option<&str>, line: Option<i64>) -> String {
    match kind {
        LinkKind::Doc => match anchor {
            Some(a) => format!("d{id}#{a}"),
            None => format!("d{id}"),
        },
        LinkKind::Task => format!("t{id}"),
        LinkKind::Message => format!("#{id}"),
        LinkKind::Thread => format!("thread {id}"),
        LinkKind::Seat => format!("@{id}"),
        LinkKind::Path => match line {
            Some(l) => format!("{id}:{l}"),
            None => id.to_string(),
        },
    }
}

/// A parsed body.
#[derive(Debug, Clone, Default)]
pub struct Parsed {
    pub sections: Vec<Section>,
    pub links: Vec<Link>,
    pub tokens: i64,
}

impl Parsed {
    /// The section with this anchor.
    pub fn section(&self, anchor: &str) -> Option<&Section> {
        self.sections.iter().find(|s| s.anchor == anchor)
    }
    /// The anchor of the section that holds byte `offset` (the innermost).
    pub fn anchor_at(&self, offset: usize) -> &str {
        self.sections
            .iter()
            .filter(|s| s.start <= offset && offset < s.own_end.max(s.start + 1))
            .map(|s| s.anchor.as_str())
            .next_back()
            .unwrap_or("")
    }
}

fn options() -> Options {
    Options::ENABLE_HEADING_ATTRIBUTES
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
        | Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
}

/// Parse a body: sections (anchors as written with `{#id}`, else derived
/// from the heading text) and links (not in code blocks).
pub fn parse(body: &str) -> Parsed {
    struct H {
        level: i64,
        id: Option<String>,
        text: String,
        start: usize,
        end: usize,
    }
    let mut heads: Vec<H> = Vec::new();
    let mut cur: Option<H> = None;
    let mut links = Vec::new();
    let mut in_code_block = 0usize;
    // Adjacent text events are scanned together (the parser splits text at
    // brackets and other characters).
    let mut text_buf = String::new();
    let mut text_start = 0usize;
    let flush = |buf: &mut String, start: usize, links: &mut Vec<Link>| {
        if !buf.is_empty() {
            scan_text(buf, start, links);
            buf.clear();
        }
    };
    for (ev, range) in Parser::new_ext(body, options()).into_offset_iter() {
        match &ev {
            Event::Text(t) if in_code_block == 0 => {
                if let Some(h) = cur.as_mut() {
                    h.text.push_str(t);
                }
                if text_buf.is_empty() {
                    text_start = range.start;
                }
                text_buf.push_str(t);
                continue;
            }
            _ => flush(&mut text_buf, text_start, &mut links),
        }
        match ev {
            Event::Start(Tag::Heading { level, id, .. }) => {
                cur = Some(H {
                    level: level as i64,
                    id: id.map(|s| s.to_string()),
                    text: String::new(),
                    start: range.start,
                    end: range.end,
                });
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some(h) = cur.take() {
                    heads.push(h);
                }
            }
            Event::Start(Tag::CodeBlock(_)) | Event::Start(Tag::MetadataBlock(_)) => {
                in_code_block += 1
            }
            Event::End(TagEnd::CodeBlock) | Event::End(TagEnd::MetadataBlock(_)) => {
                in_code_block = in_code_block.saturating_sub(1)
            }
            Event::Code(t) => {
                if let Some(h) = cur.as_mut() {
                    h.text.push_str(&t);
                }
                scan_text(&t, range.start, &mut links);
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                let d = dest_url.trim();
                if let Some(rest) = d.strip_prefix("doc:") {
                    let rest = rest.strip_prefix('d').unwrap_or(rest);
                    let (num, anchor) = match rest.split_once('#') {
                        Some((n, a)) => (n, Some(a.to_string()).filter(|a| !a.is_empty())),
                        None => (rest, None),
                    };
                    if let Ok(n) = num.parse::<i64>()
                        && n > 0
                    {
                        links.push(Link {
                            kind: LinkKind::Doc,
                            id: n.to_string(),
                            num: Some(n),
                            anchor,
                            line: None,
                            offset: range.start,
                        });
                    }
                } else if !d.contains("://") && !d.starts_with('#') && !d.starts_with("mailto:") {
                    scan_text(d, range.start, &mut links);
                }
            }
            _ => {}
        }
    }
    flush(&mut text_buf, text_start, &mut links);

    // Anchors: explicit ids as written, else slugs with `-1`, `-2` for
    // duplicates.
    let mut used: HashSet<String> = heads.iter().filter_map(|h| h.id.clone()).collect();
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut sections = Vec::new();
    let first = heads.first().map_or(body.len(), |h| h.start);
    if !body[..first].trim().is_empty() {
        sections.push(Section {
            level: 0,
            anchor: String::new(),
            heading: String::new(),
            path: String::new(),
            start: 0,
            end: first,
            own_end: first,
            heading_end: 0,
            tokens: token_estimate(&body[..first]),
        });
    }
    let mut stack: Vec<(i64, String)> = Vec::new();
    for (i, h) in heads.iter().enumerate() {
        let heading = h.text.trim().to_string();
        let anchor = match &h.id {
            Some(id) => id.clone(),
            None => {
                let base = slugify(&heading);
                let base = if base.is_empty() {
                    "section".to_string()
                } else {
                    base
                };
                let mut n = counts.get(&base).copied().unwrap_or(0);
                let mut a = if n == 0 {
                    base.clone()
                } else {
                    format!("{base}-{n}")
                };
                while used.contains(&a) {
                    n += 1;
                    a = format!("{base}-{n}");
                }
                counts.insert(base, n + 1);
                used.insert(a.clone());
                a
            }
        };
        while stack.last().is_some_and(|(l, _)| *l >= h.level) {
            stack.pop();
        }
        stack.push((h.level, heading.clone()));
        let path = stack
            .iter()
            .map(|(_, t)| t.as_str())
            .collect::<Vec<_>>()
            .join(" > ");
        let end = heads[i + 1..]
            .iter()
            .find(|n| n.level <= h.level)
            .map_or(body.len(), |n| n.start);
        let own_end = heads.get(i + 1).map_or(body.len(), |n| n.start);
        sections.push(Section {
            level: h.level,
            anchor,
            heading,
            path,
            start: h.start,
            end,
            own_end,
            heading_end: body[..h.end.min(own_end)]
                .trim_end_matches(['\n', '\r'])
                .len()
                .max(h.start),
            tokens: token_estimate(&body[h.start..end]),
        });
    }
    // One link per target and section.
    let mut seen = HashSet::new();
    let parsed = Parsed {
        sections,
        links: Vec::new(),
        tokens: token_estimate(body),
    };
    let mut out = Vec::new();
    for l in links {
        let key = (
            parsed.anchor_at(l.offset).to_string(),
            l.kind,
            l.id.clone(),
            l.anchor.clone(),
            l.line,
        );
        if seen.insert(key) {
            out.push(l);
        }
    }
    Parsed {
        links: out,
        ..parsed
    }
}

/// A heading's anchor derived from its text, GitHub style: lower case,
/// letters (any script), digits, `-` and `_` kept, spaces to `-`, the rest
/// dropped.
pub fn slugify(text: &str) -> String {
    let mut out = String::new();
    for c in text.trim().chars() {
        if c.is_alphanumeric() || c == '-' || c == '_' {
            out.extend(c.to_lowercase());
        } else if c.is_whitespace() {
            out.push('-');
        }
    }
    out
}

/// CJK characters (Han, kana, Hangul, CJK punctuation and full-width
/// forms) count one token each.
pub fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3000..=0x303F | 0x3040..=0x30FF | 0x3100..=0x312F | 0x3130..=0x318F
        | 0x31A0..=0x31FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xAC00..=0xD7AF
        | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFFEF | 0x20000..=0x3FFFF)
}

/// The token estimate: one token per 4 characters of other text, one per
/// CJK character, rounded up.
pub fn token_estimate(s: &str) -> i64 {
    let (mut cjk, mut other) = (0i64, 0i64);
    for c in s.chars() {
        if is_cjk(c) {
            cjk += 1;
        } else {
            other += 1;
        }
    }
    cjk + (other + 3) / 4
}

/// `52,000`.
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

/// The required section titles (case-insensitive) that no heading of the
/// body has.
pub fn missing_sections(p: &Parsed, required: &[String]) -> Vec<String> {
    let have: HashSet<String> = p
        .sections
        .iter()
        .filter(|s| s.level > 0)
        .map(|s| s.heading.to_lowercase())
        .collect();
    required
        .iter()
        .filter(|r| !have.contains(&r.trim().to_lowercase()))
        .cloned()
        .collect()
}

pub fn sha256_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

/// A unified diff of two texts (3 lines of context).
pub fn unified_diff(a: &str, b: &str, from: &str, to: &str) -> String {
    similar::TextDiff::from_lines(a, b)
        .unified_diff()
        .context_radius(3)
        .header(from, to)
        .to_string()
}

fn trigrams(s: &str) -> HashSet<[char; 3]> {
    let mut out = HashSet::new();
    let lower = s.to_lowercase();
    for w in lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        let chars: Vec<char> = [' ', ' ']
            .into_iter()
            .chain(w.chars())
            .chain([' '])
            .collect();
        for t in chars.windows(3) {
            out.insert([t[0], t[1], t[2]]);
        }
    }
    out
}

/// Trigram similarity as `pg_trgm` computes it: shared trigrams of the
/// words (padded with two spaces before and one after) over all trigrams.
pub fn similarity(a: &str, b: &str) -> f64 {
    let (x, y) = (trigrams(a), trigrams(b));
    if x.is_empty() || y.is_empty() {
        return 0.0;
    }
    let shared = x.intersection(&y).count() as f64;
    shared / (x.len() as f64 + y.len() as f64 - shared)
}

// --- scanning text for references -----------------------------------------------

fn word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn anchor_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_' || (!c.is_ascii() && c.is_alphanumeric())
}

fn path_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | '-')
}

/// A reference at byte `i` of `s` (a word boundary): its kind, id, number,
/// anchor, line and the byte where it ends.
type Found = (
    LinkKind,
    String,
    Option<i64>,
    Option<String>,
    Option<i64>,
    usize,
);

fn digits_at(s: &str, i: usize) -> Option<(i64, usize)> {
    let n = s[i..].bytes().take_while(u8::is_ascii_digit).count();
    if n == 0 || n > 18 {
        return None;
    }
    let v: i64 = s[i..i + n].parse().ok()?;
    (v > 0).then_some((v, i + n))
}

fn boundary_after(s: &str, j: usize) -> bool {
    s[j..].chars().next().is_none_or(|c| !word_char(c))
}

/// A path at `i`: path characters with a `/` and a file extension,
/// optionally `:line`. URLs (`//` or after `:`) are not paths.
fn path_at(s: &str, i: usize) -> Option<Found> {
    let run: String = s[i..].chars().take_while(|c| path_char(*c)).collect();
    let mut p = run.trim_end_matches(['.', '-', '/']).to_string();
    if p.contains("//") || !p.contains('/') || p.starts_with('/') && p.len() < 2 {
        return None;
    }
    if s[..i].ends_with([':', '/']) {
        return None;
    }
    let last = p.rsplit('/').next().unwrap_or_default();
    let ext = last.rsplit_once('.');
    match ext {
        Some((stem, e))
            if !e.is_empty()
                && e.chars().all(|c| c.is_ascii_alphanumeric())
                && !stem.is_empty() => {}
        _ => return None,
    }
    let mut end = i + p.len();
    let mut line = None;
    if s[end..].starts_with(':')
        && let Some((n, j)) = digits_at(s, end + 1)
        && boundary_after(s, j)
    {
        line = Some(n);
        end = j;
    }
    p = p.trim_start_matches("./").to_string();
    Some((LinkKind::Path, p, None, None, line, end))
}

fn ref_at(s: &str, i: usize) -> Option<Found> {
    let rest = &s[i..];
    let c = rest.chars().next()?;
    match c {
        'd' => {
            let (n, mut j) = digits_at(s, i + 1)?;
            let mut anchor = None;
            if s[j..].starts_with('#') {
                let a: String = s[j + 1..].chars().take_while(|c| anchor_char(*c)).collect();
                let a = a.trim_end_matches(['-', '_']).to_string();
                if !a.is_empty() {
                    j += 1 + a.len();
                    anchor = Some(a);
                }
            }
            boundary_after(s, j).then(|| (LinkKind::Doc, n.to_string(), Some(n), anchor, None, j))
        }
        't' => {
            if rest.len() > 7 && rest[..7].eq_ignore_ascii_case("thread ") {
                let (n, j) = digits_at(s, i + 7)?;
                return boundary_after(s, j)
                    .then(|| (LinkKind::Thread, n.to_string(), Some(n), None, None, j));
            }
            let (n, j) = digits_at(s, i + 1)?;
            boundary_after(s, j).then(|| (LinkKind::Task, n.to_string(), Some(n), None, None, j))
        }
        'T' if rest.len() > 7 && rest[..7].eq_ignore_ascii_case("thread ") => {
            let (n, j) = digits_at(s, i + 7)?;
            boundary_after(s, j).then(|| (LinkKind::Thread, n.to_string(), Some(n), None, None, j))
        }
        '#' => {
            if i > 0 && s[..i].ends_with('#') {
                return None;
            }
            let (n, j) = digits_at(s, i + 1)?;
            boundary_after(s, j).then(|| (LinkKind::Message, n.to_string(), Some(n), None, None, j))
        }
        '@' => {
            let name: String = s[i + 1..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
                .collect();
            let name = name.trim_end_matches(['.', '-', '_']).to_string();
            if name.is_empty()
                || !name
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphanumeric())
            {
                return None;
            }
            let j = i + 1 + name.len();
            // `name@org` addresses and e-mail addresses are not seat links.
            if s[j..].starts_with('@') {
                return None;
            }
            Some((LinkKind::Seat, name.to_lowercase(), None, None, None, j))
        }
        _ => None,
    }
}

/// Every reference in `s` (at byte `base` of the body).
fn scan_text(s: &str, base: usize, out: &mut Vec<Link>) {
    for (kind, id, num, anchor, line, start) in scan(s) {
        out.push(Link {
            kind,
            id,
            num,
            anchor,
            line,
            offset: base + start,
        });
    }
}

/// References in `s`; the last field is the start byte (not the end).
fn scan(s: &str) -> Vec<Found> {
    scan_spans(s)
        .into_iter()
        .map(|(i, _, f)| (f.0, f.1, f.2, f.3, f.4, i))
        .collect()
}

/// References in `s`: start byte, end byte and what was found.
fn scan_spans(s: &str) -> Vec<(usize, usize, Found)> {
    let mut out = Vec::new();
    let mut i = 0;
    let mut prev: Option<char> = None;
    while i < s.len() {
        let c = s[i..].chars().next().unwrap_or(' ');
        let at_boundary = prev.is_none_or(|p| !word_char(p) && p != '@');
        if at_boundary {
            let found = if path_char(c) && c != '-' && c != '.' {
                path_at(s, i).or_else(|| ref_at(s, i))
            } else {
                ref_at(s, i)
            };
            if let Some(f) = found {
                let end = f.5;
                out.push((i, end, f));
                prev = s[..end].chars().next_back();
                i = end;
                continue;
            }
        }
        // Skip a whole run of path characters that is not a reference (so
        // that `docs/d12.md` and `abc-d12` do not yield `d12`).
        if path_char(c) {
            let run = s[i..]
                .chars()
                .take_while(|c| path_char(*c) || *c == '#')
                .count();
            let len: usize = s[i..].chars().take(run).map(char::len_utf8).sum();
            if len > 0 && !at_boundary {
                prev = s[..i + len].chars().next_back();
                i += len;
                continue;
            }
        }
        prev = Some(c);
        i += c.len_utf8();
    }
    out
}

/// The numbers of the documents `text` names (`d12`, `d12#x`), first
/// occurrence first, at most `max`.
pub fn doc_refs(text: &str, max: usize) -> Vec<i64> {
    let mut out = Vec::new();
    for (_, _, (kind, _, num, _, _, _)) in scan_spans(text) {
        if kind == LinkKind::Doc
            && let Some(n) = num
            && !out.contains(&n)
        {
            out.push(n);
            if out.len() == max {
                break;
            }
        }
    }
    out
}

/// Rewrite document references (`dN`, `dN#anchor`) in a body: `f` gets the
/// number and anchor of each and answers the replacement text, or `None`
/// to keep it. Answers the new body and whether it changed.
pub fn rewrite_doc_refs(
    body: &str,
    f: &dyn Fn(i64, Option<&str>) -> Option<String>,
) -> (String, bool) {
    let mut out = String::with_capacity(body.len());
    let mut last = 0;
    let mut changed = false;
    for (start, end, (kind, _, num, anchor, _, _)) in scan_spans(body) {
        if kind != LinkKind::Doc {
            continue;
        }
        let Some(n) = num else { continue };
        if let Some(r) = f(n, anchor.as_deref()) {
            out.push_str(&body[last..start]);
            out.push_str(&r);
            last = end;
            changed = true;
        }
    }
    out.push_str(&body[last..]);
    (out, changed)
}

/// Shift the level of every ATX heading (outside fenced code) by `by`,
/// at most to level 6.
pub fn shift_headings(body: &str, by: usize) -> String {
    let mut out = String::with_capacity(body.len() + 64);
    let mut fence: Option<String> = None;
    for line in body.split_inclusive('\n') {
        let t = line.trim_start();
        if let Some(f) = &fence {
            if t.starts_with(f.as_str()) {
                fence = None;
            }
            out.push_str(line);
            continue;
        }
        if t.starts_with("```") || t.starts_with("~~~") {
            fence = Some(t[..3].to_string());
            out.push_str(line);
            continue;
        }
        let hashes = t.chars().take_while(|c| *c == '#').count();
        let after = t[hashes..].chars().next();
        if (1..=6).contains(&hashes)
            && after.is_none_or(|c| c == ' ' || c == '\t' || c == '\n' || c == '\r')
        {
            let n = (hashes + by).min(6);
            out.push_str(&"#".repeat(n));
            out.push_str(&t[hashes..]);
        } else {
            out.push_str(line);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections_and_anchors() {
        let body = "Intro text.\n\n# Deploy\n\nSteps.\n\n## Rollback\n\nUndo.\n\n## Rollback\n\nAgain.\n\n# Keep {#kept}\n\nx\n\n# 部署 说明\n\ny\n";
        let p = parse(body);
        let anchors: Vec<&str> = p.sections.iter().map(|s| s.anchor.as_str()).collect();
        assert_eq!(
            anchors,
            ["", "deploy", "rollback", "rollback-1", "kept", "部署-说明"]
        );
        let r = p.section("rollback").unwrap();
        assert_eq!(r.path, "Deploy > Rollback");
        assert_eq!(r.heading, "Rollback");
        assert!(body[r.start..r.end].starts_with("## Rollback\n\nUndo."));
        assert!(!body[r.start..r.end].contains("Again"));
        let d = p.section("deploy").unwrap();
        assert!(body[d.start..d.end].contains("Again") && !body[d.start..d.end].contains("Keep"));
        assert_eq!(p.section("kept").unwrap().heading, "Keep");
    }

    #[test]
    fn links_found() {
        let body = "See d12 and d3#roll-back, [[d40]], [x](doc:d41#a), t7, #99, thread 5, @w1.\n\
                    Files: `crates/a/src/lib.rs:10` and docs/d12.md; not https://x.org/a/b.html, \
                    md5, abc-d9, w1@org, me@example.com.\n\n```\nd77 in code\n```\n";
        let p = parse(body);
        let shown: Vec<String> = p.links.iter().map(Link::shown).collect();
        assert_eq!(
            shown,
            [
                "d12",
                "d3#roll-back",
                "d40",
                "d41#a",
                "t7",
                "#99",
                "thread 5",
                "@w1",
                "crates/a/src/lib.rs:10",
                "docs/d12.md"
            ],
            "{shown:?}"
        );
    }

    #[test]
    fn tokens() {
        assert_eq!(token_estimate(""), 0);
        assert_eq!(token_estimate("abcd"), 1);
        assert_eq!(token_estimate("abcde"), 2);
        assert_eq!(token_estimate("部署"), 2);
        assert_eq!(token_estimate("部署 ab"), 3);
        assert_eq!(thousands(52000), "52,000");
    }

    #[test]
    fn required() {
        let p = parse("# Context\n\n# decision\n");
        let req: Vec<String> = ["Context", "Decision", "Alternatives"]
            .map(String::from)
            .to_vec();
        assert_eq!(missing_sections(&p, &req), ["Alternatives"]);
    }

    #[test]
    fn rewriting() {
        let (b, ch) = rewrite_doc_refs("a d12#x b d12 c d120 docs/d12#x.md d12#xy", &|n, a| {
            (n == 12 && a == Some("x")).then(|| "d31".to_string())
        });
        assert!(ch);
        assert_eq!(b, "a d31 b d12 c d120 docs/d12#x.md d12#xy");
        assert_eq!(
            shift_headings("# A\ntext\n```\n# no\n```\n## B\n", 1),
            "## A\ntext\n```\n# no\n```\n### B\n"
        );
    }

    #[test]
    fn similar_titles() {
        assert!(similarity("Deploy rollback procedure", "deploy rollback") > 0.5);
        assert!(similarity("Deploy rollback", "Database schema") < 0.2);
        assert_eq!(similarity("", "x"), 0.0);
    }
}
