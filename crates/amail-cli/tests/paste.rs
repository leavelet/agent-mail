//! The terminal-paste wake-up against real zellij panes running a stand-in
//! TUI (`common/fake_tui.py`): the screen checks pass only for an empty
//! input box, and what is pasted and submitted is exactly the nudge.
//! Skipped (with a notice) when `zellij` or `python3` is not installed.
//! Uses a zellij session of its own, removed at the end.

use amail_node::mux::{Mux, PaneSpec, Zellij};
use std::path::Path;
use std::time::{Duration, Instant};

fn have(bin: &str) -> bool {
    std::process::Command::new(bin)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

async fn wait_screen(z: &Zellij, session: &str, pane: u32, needle: &str) -> String {
    let t0 = Instant::now();
    loop {
        let s = z.screen(session, pane).await.unwrap_or_default();
        if s.contains(needle) {
            return s;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(20),
            "screen never showed {needle:?}:\n{s}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn log(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn paste_into_real_panes() {
    if !have("zellij") || !have("python3") {
        eprintln!("SKIPPED paste_into_real_panes: zellij or python3 is missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/common/fake_tui.py");
    let z = Zellij::new("zellij", dir.path(), 200, 50);
    let session = format!("amail-pastetest-{:08x}", rand_u32());
    let mut panes = Vec::new();
    for mode in ["empty", "draft", "dialog"] {
        let logf = dir.path().join(format!("{mode}.log"));
        let spec = PaneSpec {
            name: mode.into(),
            cwd: dir.path().to_path_buf(),
            argv: vec![
                "python3".into(),
                script.to_string_lossy().into_owned(),
                logf.to_string_lossy().into_owned(),
                mode.into(),
            ],
        };
        let pane = z.open(&session, mode, &spec).await.unwrap();
        panes.push((mode, pane, logf));
    }
    let result = async {
        let nudge = "[amail] You have new mail (wake #7).";
        for (mode, pane, logf) in &panes {
            let screen = wait_screen(&z, &session, *pane, "● Done").await;
            let check = amail_node::paste::check(&screen);
            if *mode != "empty" {
                assert!(check.is_err(), "{mode}: {screen}");
                continue;
            }
            assert_eq!(check, Ok(()), "{screen}");
            z.paste(&session, *pane, nudge).await.unwrap();
            let screen = wait_screen(&z, &session, *pane, nudge).await;
            assert_eq!(
                amail_node::paste::check_typed(&screen, nudge),
                Ok(()),
                "{screen}"
            );
            z.enter(&session, *pane).await.unwrap();
            let t0 = Instant::now();
            while !log(logf).contains("SUBMIT ") {
                assert!(
                    t0.elapsed() < Duration::from_secs(10),
                    "no submit: {}",
                    log(logf)
                );
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            let l = log(logf);
            eprintln!("{mode} pane received:\n{l}");
            assert!(l.contains(&format!("SUBMIT {nudge}\n")), "{l}");
            // The paste arrived as one bracketed paste.
            assert!(l.contains("\\x1b[200~"), "{l}");
        }
        // Nothing reached the panes that were refused.
        for (mode, _, logf) in &panes {
            if *mode != "empty" {
                assert!(!log(logf).contains("RECV"), "{mode}: {}", log(logf));
            }
        }
    }
    .await;
    let _ = z.kill_session(&session).await;
    result
}

fn rand_u32() -> u32 {
    let mut b = [0u8; 4];
    use std::io::Read;
    std::fs::File::open("/dev/urandom")
        .unwrap()
        .read_exact(&mut b)
        .unwrap();
    u32::from_le_bytes(b)
}
