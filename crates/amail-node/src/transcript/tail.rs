//! Following one transcript file as it grows.
//!
//! [`Tail::open`] reads the end of the file (at most [`Limits::window`]
//! bytes) and returns the last few events; [`Tail::poll`] then returns what
//! was appended since. It copes with:
//!
//! - a last line still being written: bytes after the last newline are kept
//!   until the line is complete;
//! - truncation (the file got shorter than what was read): start over from
//!   the beginning, reported as a reset;
//! - replacement or rotation (the path now names another file): start over
//!   with the new file, reported as a reset;
//! - deletion: reported as missing, and followed again once the path exists;
//! - overlong lines (over [`Limits::max_line`]): skipped and counted.
//!
//! Plain polling with `std::fs`; callers run it off the async executor.

use super::{ParseStats, Parser, parser_for};
use crate::adapter::Harness;
use amail_proto::TranscriptEvent;
use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// Size limits of a [`Tail`].
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// How much of the end of a file the backfill reads.
    pub window: u64,
    /// Longest line kept; longer ones are skipped.
    pub max_line: usize,
    /// Most bytes one poll reads (the rest comes with the next poll).
    pub max_read: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            window: 16 << 20,
            max_line: 8 << 20,
            max_read: 16 << 20,
        }
    }
}

/// What one [`Tail::poll`] found.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PollResult {
    /// Forget earlier events: the file was truncated or replaced, and
    /// `events` is a fresh backfill.
    pub reset: bool,
    pub events: Vec<TranscriptEvent>,
    /// The path does not exist (now).
    pub missing: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TailStats {
    pub parse: ParseStats,
    /// Lines skipped for being longer than [`Limits::max_line`].
    pub long_lines: u64,
    /// Times the file was read again from the start.
    pub resets: u64,
}

pub struct Tail {
    path: PathBuf,
    harness: Harness,
    backfill: usize,
    limits: Limits,
    parser: Box<dyn Parser>,
    file: File,
    /// `(dev, ino)` of `file`.
    id: (u64, u64),
    /// Bytes of `file` consumed so far.
    offset: u64,
    partial: Vec<u8>,
    /// Inside an overlong line: drop bytes up to the next newline.
    skipping: bool,
    long_lines: u64,
    resets: u64,
    missing: bool,
}

impl std::fmt::Debug for Tail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tail")
            .field("path", &self.path)
            .field("offset", &self.offset)
            .field("missing", &self.missing)
            .finish()
    }
}

fn file_id(m: &std::fs::Metadata) -> (u64, u64) {
    (m.dev(), m.ino())
}

impl Tail {
    /// Start following `path`: returns the follower and the last `backfill`
    /// events of the file. Fails if the file cannot be opened.
    pub fn open(
        path: impl AsRef<Path>,
        harness: Harness,
        backfill: usize,
    ) -> io::Result<(Tail, Vec<TranscriptEvent>)> {
        Self::open_with(path, harness, backfill, Limits::default())
    }

    pub fn open_with(
        path: impl AsRef<Path>,
        harness: Harness,
        backfill: usize,
        limits: Limits,
    ) -> io::Result<(Tail, Vec<TranscriptEvent>)> {
        let path = path.as_ref().to_path_buf();
        let file = File::open(&path)?;
        let id = file_id(&file.metadata()?);
        let mut t = Tail {
            path,
            harness,
            backfill,
            limits,
            parser: parser_for(harness),
            file,
            id,
            offset: 0,
            partial: Vec::new(),
            skipping: false,
            long_lines: 0,
            resets: 0,
            missing: false,
        };
        let events = t.load()?;
        Ok((t, events))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn stats(&self) -> TailStats {
        TailStats {
            parse: self.parser.stats(),
            long_lines: self.long_lines,
            resets: self.resets,
        }
    }

    /// Read the end of the current file afresh; returns the backfill.
    fn load(&mut self) -> io::Result<Vec<TranscriptEvent>> {
        let size = self.file.metadata()?.len();
        let start = size.saturating_sub(self.limits.window);
        self.partial.clear();
        // Starting mid-file: the first (partial) line is dropped.
        self.skipping = start > 0;
        self.offset = start;
        self.file.seek(SeekFrom::Start(start))?;
        let mut keep: VecDeque<TranscriptEvent> = VecDeque::new();
        let backfill = self.backfill;
        self.read_to(size, |e| {
            if backfill == 0 {
                return;
            }
            if keep.len() == backfill {
                keep.pop_front();
            }
            keep.push_back(e);
        })?;
        Ok(keep.into())
    }

    /// Consume the file from `offset` up to `end`, passing each event to `f`.
    fn read_to(&mut self, end: u64, mut f: impl FnMut(TranscriptEvent)) -> io::Result<()> {
        let mut buf = vec![0u8; 64 * 1024];
        while self.offset < end {
            let want = ((end - self.offset) as usize).min(buf.len());
            let n = self.file.read(&mut buf[..want])?;
            if n == 0 {
                break;
            }
            self.offset += n as u64;
            self.feed(&buf[..n], &mut f);
        }
        Ok(())
    }

    fn feed(&mut self, mut bytes: &[u8], f: &mut impl FnMut(TranscriptEvent)) {
        while let Some(nl) = bytes.iter().position(|&b| b == b'\n') {
            let (head, rest) = (&bytes[..nl], &bytes[nl + 1..]);
            bytes = rest;
            if self.skipping {
                self.skipping = false;
                self.partial.clear();
                continue;
            }
            if self.partial.len() + head.len() > self.limits.max_line {
                self.partial.clear();
                self.long_lines += 1;
                continue;
            }
            let line = if self.partial.is_empty() {
                String::from_utf8_lossy(head).into_owned()
            } else {
                self.partial.extend_from_slice(head);
                let s = String::from_utf8_lossy(&self.partial).into_owned();
                self.partial.clear();
                s
            };
            for e in self.parser.line(&line) {
                f(e);
            }
        }
        if self.skipping {
            return;
        }
        if self.partial.len() + bytes.len() > self.limits.max_line {
            self.partial.clear();
            self.long_lines += 1;
            self.skipping = true;
        } else {
            self.partial.extend_from_slice(bytes);
        }
    }

    /// Start over with whatever the path names now.
    fn reset(&mut self, file: File, id: (u64, u64)) -> io::Result<PollResult> {
        self.file = file;
        self.id = id;
        self.parser = parser_for(self.harness);
        self.resets += 1;
        self.missing = false;
        let events = self.load()?;
        Ok(PollResult {
            reset: true,
            events,
            missing: false,
        })
    }

    /// New events since the last call.
    pub fn poll(&mut self) -> io::Result<PollResult> {
        let meta = match std::fs::metadata(&self.path) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.missing = true;
                return Ok(PollResult {
                    missing: true,
                    ..Default::default()
                });
            }
            Err(e) => return Err(e),
        };
        let id = file_id(&meta);
        if id != self.id || self.missing {
            let file = match File::open(&self.path) {
                Ok(f) => f,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    self.missing = true;
                    return Ok(PollResult {
                        missing: true,
                        ..Default::default()
                    });
                }
                Err(e) => return Err(e),
            };
            let id = file_id(&file.metadata()?);
            return self.reset(file, id);
        }
        let size = self.file.metadata()?.len();
        if size < self.offset {
            // Truncated (and maybe rewritten): read it again.
            let file = File::open(&self.path)?;
            let id = file_id(&file.metadata()?);
            return self.reset(file, id);
        }
        let mut events = Vec::new();
        if size > self.offset {
            let end = size.min(self.offset + self.limits.max_read);
            self.file.seek(SeekFrom::Start(self.offset))?;
            self.read_to(end, |e| events.push(e))?;
        }
        Ok(PollResult {
            reset: false,
            events,
            missing: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn user(text: &str) -> String {
        format!(
            "{}\n",
            serde_json::json!({"type": "user", "message": {"role": "user", "content": text}})
        )
    }

    fn texts(evs: &[TranscriptEvent]) -> Vec<&str> {
        evs.iter().map(|e| e.text.as_str()).collect()
    }

    fn append(path: &Path, s: &str) {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(path)
            .unwrap();
        f.write_all(s.as_bytes()).unwrap();
    }

    #[test]
    fn backfill_limit_and_growth_with_partial_line() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.jsonl");
        let mut s = String::new();
        for i in 0..10 {
            s.push_str(&user(&format!("m{i}")));
        }
        std::fs::write(&p, &s).unwrap();
        let (mut t, back) = Tail::open(&p, Harness::Claude, 3).unwrap();
        assert_eq!(texts(&back), ["m7", "m8", "m9"]);
        assert_eq!(t.poll().unwrap(), PollResult::default());

        // A line written in two pieces.
        let line = user("half and half");
        let (a, b) = line.split_at(20);
        append(&p, a);
        assert!(t.poll().unwrap().events.is_empty());
        append(&p, b);
        append(&p, &user("next"));
        let r = t.poll().unwrap();
        assert!(!r.reset);
        assert_eq!(texts(&r.events), ["half and half", "next"]);

        // Garbage lines are skipped and counted, not fatal.
        append(&p, "{oops\n");
        append(&p, &user("after"));
        assert_eq!(texts(&t.poll().unwrap().events), ["after"]);
        assert_eq!(t.stats().parse.skipped, 1);
    }

    #[test]
    fn truncation_resets() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.jsonl");
        std::fs::write(&p, user("one") + &user("two")).unwrap();
        let (mut t, back) = Tail::open(&p, Harness::Claude, 10).unwrap();
        assert_eq!(texts(&back), ["one", "two"]);
        // Same inode, shorter content.
        std::fs::OpenOptions::new()
            .write(true)
            .open(&p)
            .unwrap()
            .set_len(0)
            .unwrap();
        append(&p, &user("fresh"));
        let r = t.poll().unwrap();
        assert!(r.reset);
        assert_eq!(texts(&r.events), ["fresh"]);
        assert_eq!(t.stats().resets, 1);
        append(&p, &user("more"));
        assert_eq!(texts(&t.poll().unwrap().events), ["more"]);
    }

    #[test]
    fn replacement_by_rename_resets() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.jsonl");
        std::fs::write(&p, user("old 1") + &user("old 2") + &user("old 3")).unwrap();
        let (mut t, _) = Tail::open(&p, Harness::Claude, 10).unwrap();
        let tmp = dir.path().join("new.jsonl");
        // Longer than the old file, so only the inode tells them apart.
        std::fs::write(
            &tmp,
            user("new 1") + &user("new 2") + &user("new 3") + &user("new 4"),
        )
        .unwrap();
        std::fs::rename(&tmp, &p).unwrap();
        let r = t.poll().unwrap();
        assert!(r.reset);
        assert_eq!(texts(&r.events), ["new 1", "new 2", "new 3", "new 4"]);
        append(&p, &user("new 5"));
        assert_eq!(texts(&t.poll().unwrap().events), ["new 5"]);
    }

    #[test]
    fn deletion_and_recreation() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.jsonl");
        std::fs::write(&p, user("a")).unwrap();
        let (mut t, _) = Tail::open(&p, Harness::Claude, 10).unwrap();
        std::fs::remove_file(&p).unwrap();
        let r = t.poll().unwrap();
        assert!(r.missing && r.events.is_empty());
        assert!(t.poll().unwrap().missing);
        std::fs::write(&p, user("b")).unwrap();
        let r = t.poll().unwrap();
        assert!(r.reset && !r.missing);
        assert_eq!(texts(&r.events), ["b"]);
        assert!(Tail::open(dir.path().join("none"), Harness::Claude, 1).is_err());
    }

    #[test]
    fn overlong_lines_and_window() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.jsonl");
        let limits = Limits {
            window: 200,
            max_line: 100,
            max_read: 1 << 20,
        };
        // Older lines beyond the window are not read; the line cut by the
        // window start is dropped.
        let mut s = String::new();
        for i in 0..20 {
            s.push_str(&user(&format!("w{i:02}")));
        }
        std::fs::write(&p, &s).unwrap();
        let (mut t, back) = Tail::open_with(&p, Harness::Claude, 100, limits).unwrap();
        assert!(!back.is_empty() && back.len() < 20);
        assert_eq!(back.last().unwrap().text, "w19");
        assert!(back.iter().all(|e| e.text.starts_with('w')));

        // A long line, complete in one write, then one arriving in pieces.
        append(&p, &user(&"x".repeat(300)));
        append(&p, &user("ok1"));
        assert_eq!(texts(&t.poll().unwrap().events), ["ok1"]);
        let long = user(&"y".repeat(300));
        append(&p, &long[..150]);
        assert!(t.poll().unwrap().events.is_empty());
        append(&p, &long[150..]);
        append(&p, &user("ok2"));
        assert_eq!(texts(&t.poll().unwrap().events), ["ok2"]);
        assert_eq!(t.stats().long_lines, 2);
    }

    #[test]
    fn codex_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("rollout.jsonl");
        let line = serde_json::json!({"timestamp": "2026-10-04T10:00:00Z", "type": "event_msg",
            "payload": {"type": "item_completed", "item": {"type": "AgentMessage",
            "content": [{"type": "Text", "text": "hi"}]}}});
        std::fs::write(&p, format!("{line}\n")).unwrap();
        let (_, back) = Tail::open(&p, Harness::Codex, 5).unwrap();
        assert_eq!(texts(&back), ["hi"]);
    }
}
