//! The terminal-paste wake-up, a last resort: when a seat's native wake-up
//! (Claude Code's inbox socket, `codex queue`) has failed, the node may type
//! the nudge into the seat's pane and press Enter. Typing into a terminal
//! is dangerous: with a dialog open the text is dropped and Enter picks the
//! dialog's default (e.g. "Deny"); a half-written human draft would be sent
//! together with the nudge. So the node pastes only when the screen shows
//! an idle harness at an empty input box and nothing else, and presses
//! Enter only when the box then holds exactly the nudge.
//!
//! The checks read the pane's text as `zellij action dump-screen` gives it
//! (no colours), so they are conservative: anything they do not recognise
//! means "do not type".

/// How many lines at the bottom of the screen are looked at (the input box,
/// its hints, and any dialog).
const BOTTOM: usize = 14;

/// Text that marks a dialog, a question a keystroke would answer, or a
/// busy session, in the bottom lines (lower case).
const DIALOG: &[&str] = &[
    "do you want",
    "esc to cancel",
    "enter to confirm",
    "press enter",
    "(y/n)",
    "[y/n]",
    "allow",
    "deny",
    "approve",
    "trust",
    "yes, and",
    "no, and",
    "esc to interrupt",
    "ctrl+c to interrupt",
];

/// Placeholders a harness shows in an empty input box.
fn placeholder(s: &str) -> bool {
    s.starts_with("Try \"") && s.ends_with('"')
}

fn bottom(screen: &str) -> Vec<&str> {
    let lines: Vec<&str> = screen
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .collect();
    let from = lines.len().saturating_sub(BOTTOM);
    lines[from..].to_vec()
}

/// A numbered choice of a menu (`❯ 1. Yes`, `  2. No`).
fn numbered(line: &str) -> bool {
    let t = line
        .trim_start_matches(|c: char| c.is_whitespace() || "│❯>›".contains(c))
        .trim_start();
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    digits > 0 && t[digits..].starts_with(". ")
}

/// The text of the input line: the last line that starts with a prompt
/// marker (`>`, `❯`, `›`, possibly inside a box `│ > … │`).
fn input_line(lines: &[&str]) -> Option<String> {
    lines.iter().rev().find_map(|l| {
        let t = l.trim_start().trim_start_matches('│').trim_start();
        let rest = t
            .strip_prefix('>')
            .or_else(|| t.strip_prefix('❯'))
            .or_else(|| t.strip_prefix('›'))?;
        if numbered(l) {
            return None;
        }
        Some(rest.trim().trim_end_matches('│').trim().to_string())
    })
}

/// Whether it is safe to paste into this screen; the reason if not.
pub fn check(screen: &str) -> Result<(), String> {
    let lines = bottom(screen);
    let low: Vec<String> = lines.iter().map(|l| l.to_lowercase()).collect();
    if let Some(w) = DIALOG.iter().find(|w| low.iter().any(|l| l.contains(*w))) {
        return Err(format!(
            "the screen shows a dialog or a busy session ({w:?})"
        ));
    }
    if lines.iter().filter(|l| numbered(l)).count() >= 2 {
        return Err("the screen shows a menu".into());
    }
    match input_line(&lines) {
        None => Err("no input box on the screen".into()),
        Some(s) if s.is_empty() || placeholder(&s) => Ok(()),
        Some(s) => Err(format!(
            "the input box is not empty ({} characters)",
            s.chars().count()
        )),
    }
}

/// After pasting `text`: Enter is safe only if the input box holds exactly
/// it, and no dialog came up.
pub fn check_typed(screen: &str, text: &str) -> Result<(), String> {
    let lines = bottom(screen);
    let low: Vec<String> = lines.iter().map(|l| l.to_lowercase()).collect();
    if let Some(w) = DIALOG.iter().find(|w| low.iter().any(|l| l.contains(*w))) {
        return Err(format!("a dialog came up ({w:?})"));
    }
    match input_line(&lines) {
        Some(s) if s == text.trim() => Ok(()),
        Some(_) => Err("the input box does not hold exactly the nudge".into()),
        None => Err("no input box on the screen".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RULE: &str = "────────────────────────────────────────";

    fn claude(input: &str) -> String {
        format!(
            "● Done. The tests pass.\n\n{RULE}\n> {input}\n{RULE}\n  ⏵⏵ bypass permissions on (shift+tab to cycle)\n"
        )
    }

    #[test]
    fn idle_and_empty() {
        assert_eq!(check(&claude("")), Ok(()));
        assert_eq!(check(&claude("Try \"refactor main.rs\"")), Ok(()));
        let boxed = "╭──────╮\n│ >                │\n╰──────╯\n  ? for shortcuts";
        assert_eq!(check(boxed), Ok(()));
        let codex = "• Ran tests\n\n› \n\n  ⏎ send   Ctrl+J newline";
        assert_eq!(check(codex), Ok(()));
    }

    #[test]
    fn refuses() {
        // A human draft.
        assert!(check(&claude("half a sentence")).is_err());
        // Busy.
        let busy = format!("✻ Thinking… (esc to interrupt)\n{RULE}\n> \n{RULE}");
        assert!(check(&busy).is_err());
        // A permission dialog.
        let dialog = "Bash command\n  rm -rf build\nDo you want to proceed?\n❯ 1. Yes\n  2. No, and tell Claude what to do differently (esc)\n";
        assert!(check(dialog).is_err());
        // A bare menu.
        let menu = "Pick one\n❯ 1. First\n  2. Second\n";
        assert!(check(menu).is_err());
        // Nothing recognisable.
        assert!(check("$ ").is_err());
        assert!(check("").is_err());
        // Transcript text far above the box does not matter.
        let mut long = String::from("Do you want to proceed? (old output)\n");
        for i in 0..30 {
            long.push_str(&format!("line {i}\n"));
        }
        long.push_str(&claude(""));
        assert_eq!(check(&long), Ok(()));
    }

    #[test]
    fn typed() {
        let n = "[amail] You have new mail (wake #3).";
        assert_eq!(check_typed(&claude(n), n), Ok(()));
        assert!(check_typed(&claude(&format!("draft {n}")), n).is_err());
        assert!(check_typed(&claude(""), n).is_err());
    }
}
