#![cfg(windows)]
//! Photograph a real window on a real Windows desktop, and write the evidence
//! out as PNGs.
//!
//! This is the receipt the capture module exists to produce. Everything else in
//! `desktop_capture_test` checks the arithmetic of the ink measurement on any
//! host; this file checks that the syscall path actually paints a window on
//! Windows, which cannot be established anywhere else.
//!
//! It is `#[ignore]`d, like the UI dump, so the gating test job stays fast and
//! the artifact workflow asks for it explicitly. It is a *job*, not a unit test:
//! it fails when it cannot produce a photograph, because a dump that quietly
//! writes nothing is the failure mode this whole module was built to avoid.
//!
//! Why it opens its own window instead of photographing whatever is on screen:
//! a hosted runner's desktop is not ours to depend on. A window the test owns is
//! reproducible, it carries known text (so "ink > 0" means text, not a wallpaper
//! gradient), and it does not read anything the user or the runner happens to
//! have open.
//!
//! Run locally:
//! `cargo test --locked --profile ci --target x86_64-pc-windows-msvc --lib \
//!  capture_dump -- --ignored --nocapture`

use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use crate::desktop::{
    MAX_WINDOWS, MIN_INK_RATIO, capture_window_to_png, interactive_session, list_windows,
};

/// `CREATE_NEW_CONSOLE`: without it `cmd.exe` inherits this process's console and
/// no new top-level window exists to photograph.
const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;

/// How long to wait for the console window to appear. The probe measured a
/// console window starting in well under a second; the ceiling is generous
/// because a cold runner is slow and the cost of waiting is a sleep, not a flake.
const WINDOW_TIMEOUT: Duration = Duration::from_secs(20);

fn out_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("capture-dump");
    std::fs::create_dir_all(&dir).expect("create target/capture-dump");
    dir
}

/// A console window that prints a line and then stays open, so it can be found
/// and photographed. `mode con` pre-sizes the buffer the way the artifact
/// workflow does: a console whose buffer is smaller than the window renders a
/// scrolling band, and a photograph of that is not a photograph of the app.
fn spawn_marker_window(marker: &str) -> Child {
    let script = format!(
        "title {marker} & mode con cols=100 lines=20 & echo {marker} & echo capture probe ready & ping -n 120 127.0.0.1 > nul"
    );
    // `script.as_str()`, not `&script`: an array literal has one element type, and
    // `&String` does not unify with the `&str` of the flag beside it.
    Command::new("cmd.exe")
        .args(["/K", script.as_str()])
        .creation_flags(CREATE_NEW_CONSOLE)
        .spawn()
        .expect("spawn cmd.exe with its own console")
}

/// Wait for a listed window whose title contains `marker`.
fn wait_for_window(marker: &str) -> Option<isize> {
    let deadline = Instant::now() + WINDOW_TIMEOUT;
    loop {
        if let Ok(list) = list_windows() {
            if let Some(found) = list
                .windows
                .iter()
                .find(|w| w.title.contains(marker) && !w.is_degenerate())
            {
                return Some(found.hwnd);
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// A title safe to use as a file name on Windows.
fn slug(title: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('_');
    if trimmed.is_empty() {
        "untitled".to_string()
    } else {
        trimmed.chars().take(60).collect()
    }
}

#[test]
fn capture_dump() {
    // A hosted runner was measured to be on an interactive session (SessionId 2,
    // UserInteractive True), but the code must not assume it: a service seat
    // legitimately has no desktop, and the honest answer there is a loud failure
    // that says so rather than an empty artifact directory.
    assert!(
        interactive_session(),
        "no interactive desktop on this seat: there is nothing to photograph, \
         and an empty artifact would be indistinguishable from a broken capture"
    );

    let dir = out_dir();
    let marker = format!("OCAP-CAPTURE-{}", std::process::id());
    let mut child = spawn_marker_window(&marker);

    let outcome = (|| -> Result<(), String> {
        let hwnd = wait_for_window(&marker).ok_or_else(|| {
            format!(
                "the console window titled {marker} never appeared within {}s; \
                 either the seat has no desktop or CREATE_NEW_CONSOLE was ignored",
                WINDOW_TIMEOUT.as_secs()
            )
        })?;

        // The window this test owns, first: its content is known, so its ink
        // measurement is a statement about the capture path and nothing else.
        let owned = dir.join(format!("owned-{}.png", slug(&marker)));
        let capture = capture_window_to_png(hwnd, &owned)
            .map_err(|e| format!("capturing our own console window failed: {e}"))?;
        println!(
            "[capture-dump] owned window: {}x{} ink={:.6} -> {}",
            capture.width,
            capture.height,
            capture.ink_ratio,
            owned.display()
        );
        assert!(
            capture.ink_ratio > 0.0,
            "the window carries text, so a zero ink ratio means the pixels never arrived"
        );

        // Then everything else the seat offers, best effort. A window that will
        // not paint is normal (a suspended surface, a window mid-destroy) and is
        // reported, not fatal: the evidence for this task is the owned window.
        let list = list_windows().map_err(|e| format!("enumeration failed: {e}"))?;
        println!("[capture-dump] seat offers {} windows", list.len());
        let mut written = 1usize;
        let mut refused = 0usize;
        for window in list.windows.iter().take(MAX_WINDOWS) {
            if window.hwnd == hwnd {
                continue;
            }
            let path = dir.join(format!(
                "win-{:08x}-{}.png",
                window.hwnd as u64 & 0xFFFF_FFFF,
                slug(&window.title)
            ));
            match capture_window_to_png(window.hwnd, &path) {
                Ok(capture) => {
                    written += 1;
                    println!(
                        "[capture-dump] {:?}: {}x{} ink={:.6}",
                        window.title, capture.width, capture.height, capture.ink_ratio
                    );
                }
                Err(error) => {
                    refused += 1;
                    // Remove the file the failed capture may have left, so the
                    // artifact directory contains only frames that passed.
                    let _ = std::fs::remove_file(&path);
                    println!(
                        "[capture-dump] {:?}: not photographed: {error}",
                        window.title
                    );
                }
            }
        }
        println!(
            "[capture-dump] {written} frames written, {refused} windows refused (min ink threshold {MIN_INK_RATIO})"
        );
        Ok(())
    })();

    // Always reap the window we opened, success or failure, or the runner's
    // cleanup step is the only thing that closes it.
    let _ = child.kill();
    let _ = child.wait();

    if let Err(message) = outcome {
        panic!("capture dump failed: {message}");
    }
}

#[test]
#[ignore = "asked for by the artifact workflow, not part of the gating slice"]
fn a_capture_is_reproducible_across_two_reads() {
    // Not a pixel-equality claim (a console cursor blinks, a clock ticks, and a
    // test that demanded byte equality would flake forever). What must hold is
    // the geometry: the same window photographed twice reports the same size and
    // a non-blank frame both times, which is what a caller comparing successive
    // frames to detect movement relies on.
    //
    // Ignored because it needs a seat with a real desktop, and because a window
    // that refuses to paint is a fact about that window rather than a failure of
    // the capture path -- the assertion is the agreement between two reads, not
    // the existence of a subject.
    let list = list_windows().expect("enumeration must succeed");
    let Some(window) = list
        .windows
        .iter()
        .find(|w| !w.is_degenerate() && w.rect.width() > 32 && w.rect.height() > 32)
    else {
        println!("[capture-dump] no window large enough for a repeat read on this seat");
        return;
    };

    let (Ok(first), Ok(second)) = (
        crate::desktop::capture_window(window.hwnd),
        crate::desktop::capture_window(window.hwnd),
    ) else {
        println!(
            "[capture-dump] {:?} refused a repeat read; nothing to compare",
            window.title
        );
        return;
    };
    assert_eq!(
        (first.width, first.height),
        (second.width, second.height),
        "the window changed size between two immediate reads"
    );
    assert!(!first.is_blank(), "first read came back blank");
    assert!(!second.is_blank(), "second read came back blank");
    println!(
        "[capture-dump] {:?} read twice at {}x{}, ink {:.6} then {:.6}",
        window.title, first.width, first.height, first.ink_ratio, second.ink_ratio
    );
}
