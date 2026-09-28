#![cfg(windows)]
//! Drive a real window on a real Windows desktop, and measure what happened.
//!
//! This is the receipt the input lane exists to produce. `desktop_input_test`
//! and `desktop_input_posted_test` check the arithmetic on any host, but every
//! one of those checks would still pass if `SendInput` were called with a
//! sensible-looking event that Windows delivered somewhere else. Only a real
//! desktop can answer "did the keystroke arrive", and only a real desktop can
//! answer it with a photograph rather than a claim.
//!
//! Three separate jobs, one per claim, so a failure names the claim that broke
//! instead of the last one that ran:
//!
//! 1. a keystroke changes the photographed frame: the character reached an
//!    application and our own capture saw it. This is the end-to-end loop that
//!    makes the whole module worth having, and it uses the capture path as the
//!    sensor, so the two halves cross-check each other.
//! 2. an injected click leaves the pointer at the point that was aimed at: the
//!    0..=65535 normalisation round-trips on the machine that has to agree with
//!    it. A one-pixel error here is invisible in a unit test and lands the click
//!    on the neighbour of every button.
//! 3. a posted click leaves the pointer exactly where it was: the safety
//!    property that makes the non-intrusive route the default, measured instead
//!    of asserted from a comment.
//!
//! Each job drives a console window it opened itself, for the reason the capture
//! dump gives: a hosted runner's desktop is not ours to depend on, and a window
//! we own is one whose contents we can predict.
//!
//! `#[ignore]`d, so the gating slice stays fast and the artifact workflow asks
//! for these by name.
//!
//! Run locally:
//! `cargo test --locked --profile ci --target x86_64-pc-windows-msvc --lib \
//!  input_dump -- --ignored --nocapture`

use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use crate::desktop::{
    Delivery, MIN_INK_RATIO, MouseButton, ScreenPoint, WindowInfo, capture_window, click_window,
    cursor_position, inject_click, inject_text, interactive_session, list_windows,
};

/// `CREATE_NEW_CONSOLE`: without it `cmd.exe` inherits this process's console and
/// there is no separate window to drive.
const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;

/// How long to wait for the console to appear and to settle.
const WINDOW_TIMEOUT: Duration = Duration::from_secs(20);

/// A short pause so conhost has repainted before the second photograph. Not a
/// synchronisation primitive: Windows gives no way to ask a window whether it
/// has finished painting, so the honest options are a delay or a retry loop on
/// the measurement itself. This retries on the measurement (see
/// `capture_until_more_ink`) and the delay only sets the floor.
const PAINT_SETTLE: Duration = Duration::from_millis(250);

/// What gets typed, chosen for its pixel count rather than its meaning.
///
/// Nineteen characters is several hundred lit pixels at any console cell size,
/// which puts the change an order of magnitude above what a blinking caret can
/// account for on its own. Nothing here is a command: no Enter is ever sent, so
/// the shell keeps it as a half-typed line and throws it away when the window
/// dies.
const TYPED: &str = "ZOOM7-INPUT-RECEIPT";

fn out_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("input-dump");
    std::fs::create_dir_all(&dir).expect("create target/input-dump");
    dir
}

fn note(line: &str) {
    println!("[input-dump] {line}");
}

/// Open a console that prints a line, waits for input, and stays alive.
fn spawn_marker_window(marker: &str) -> Child {
    let script = format!(
        "title {marker} & mode con cols=100 lines=20 & echo {marker} & ping -n 120 127.0.0.1 > nul"
    );
    Command::new("cmd.exe")
        .args(["/K", script.as_str()])
        .creation_flags(CREATE_NEW_CONSOLE)
        .spawn()
        .expect("spawn cmd.exe in a new console")
}

fn stop(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Find the window this job opened, by the title only it carries.
///
/// Polling instead of a single read: the window does not exist the instant
/// `spawn` returns, and "the console never appeared" is a different finding from
/// "the title was not what we asked for".
fn wait_for_window(marker: &str) -> Result<WindowInfo, String> {
    let deadline = Instant::now() + WINDOW_TIMEOUT;
    let mut last = String::from("(no listing yet)");
    while Instant::now() < deadline {
        let list = list_windows().map_err(|e| format!("enumerate: {e}"))?;
        if let Some(window) = list.windows.iter().find(|w| w.title.contains(marker)) {
            return Ok(window.clone());
        }
        last = format!("{} windows: {}", list.windows.len(), list);
        std::thread::sleep(Duration::from_millis(200));
    }
    Err(format!(
        "no window titled *{marker}* appeared within {}s; last listing {last}",
        WINDOW_TIMEOUT.as_secs()
    ))
}

/// Capture until the frame carries at least `floor` inked-ratio, or give up.
fn capture_stable(hwnd: isize) -> Result<crate::desktop::Capture, String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let capture = capture_window(hwnd).map_err(|e| format!("capture: {e}"))?;
        if Instant::now() >= deadline {
            return Ok(capture);
        }
        // A console mid-paint can come back partly drawn; retry while it is
        // blank, and stop the moment there is something to measure.
        if capture.ink_ratio > 0.0 {
            return Ok(capture);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn write_png(name: &str, capture: &crate::desktop::Capture) -> Result<(), String> {
    let path = out_dir().join(name);
    let png = capture
        .to_png()
        .map_err(|e| format!("encode {name}: {e}"))?;
    std::fs::write(&path, png).map_err(|e| format!("write {}: {e}", path.display()))?;
    note(&format!(
        "{name}: {}x{} ink={:.6}",
        capture.width, capture.height, capture.ink_ratio
    ));
    Ok(())
}

/// 1. A keystroke reaches the window and our own capture sees it.
#[test]
#[ignore = "needs a real interactive desktop; run by the artifact workflow with --ignored"]
fn input_dump_a_keystroke_changes_the_captured_frame() {
    if !interactive_session() {
        note("no interactive seat on this host, so there is no desktop to type into");
        return;
    }
    let marker = format!("OCIN-TYPE-{}", std::process::id());
    let mut child = spawn_marker_window(&marker);
    let window = match wait_for_window(&marker) {
        Ok(window) => window,
        Err(why) => {
            stop(&mut child);
            panic!("{why}");
        }
    };

    // Injected input goes to the *focused* window, wherever that is. Refusing
    // here is the whole point of the check: without it, a window that lost focus
    // in the meantime turns this test into typing into someone else's
    // application.
    if !window.foreground {
        stop(&mut child);
        panic!(
            "the marker window lost focus before it could be typed into; injected input would \
             have gone to another application, so nothing was sent"
        );
    }

    let before = match capture_stable(window.hwnd) {
        Ok(capture) => capture,
        Err(why) => {
            stop(&mut child);
            panic!("{why} before typing");
        }
    };
    let delivery = match inject_text(TYPED) {
        Ok(delivery) => delivery,
        Err(why) => {
            stop(&mut child);
            panic!("inject_text failed: {why}");
        }
    };
    std::thread::sleep(PAINT_SETTLE);
    let after = match capture_stable(window.hwnd) {
        Ok(capture) => capture,
        Err(why) => {
            stop(&mut child);
            panic!("{why} after typing");
        }
    };
    let _ = write_png("before-type.png", &before);
    let _ = write_png("after-type.png", &after);
    stop(&mut child);

    let Delivery::Injected { events } = delivery else {
        stop(&mut child);
        panic!("inject_text must report an injected delivery");
    };
    assert_eq!(
        events,
        TYPED.len() * 2,
        "{} characters are {} press/release pairs",
        TYPED.len(),
        events
    );
    assert!(
        after.ink_ratio - before.ink_ratio > MIN_INK_RATIO,
        "typing {TYPED:?} changed the frame by {:.6} of ink ratio (before {:.6}, after {:.6}): \
         a blinking caret moves that number by a fraction of what {MIN_INK_RATIO} allows, so a \
         smaller change is not evidence that the characters arrived. Either they never reached \
         the console, or the capture is not seeing what the window paints.",
        after.ink_ratio - before.ink_ratio,
        before.ink_ratio,
        after.ink_ratio
    );
    note(&format!(
        "typed {TYPED:?}: ink {:.6} -> {:.6}, delta {:.6}, {} injected events",
        before.ink_ratio,
        after.ink_ratio,
        after.ink_ratio - before.ink_ratio,
        events
    ));
}

/// 2. An injected click puts the pointer on the point that was aimed at.
#[test]
#[ignore = "needs a real interactive desktop; run by the artifact workflow with --ignored"]
fn input_dump_an_injected_click_lands_where_it_was_aimed() {
    if !interactive_session() {
        note("no interactive seat on this host, so there is no pointer to move");
        return;
    }
    let marker = format!("OCIN-CLICK-{}", std::process::id());
    let mut child = spawn_marker_window(&marker);
    let window = match wait_for_window(&marker) {
        Ok(window) => window,
        Err(why) => {
            stop(&mut child);
            panic!("{why}");
        }
    };
    // Aim at the middle of the client area rather than the window: the point has
    // to be inside the virtual desktop (it is) and on something the click is
    // allowed to touch.
    let aimed = ScreenPoint {
        x: window.rect.left + window.rect.width() / 2,
        y: window.rect.top + window.rect.height() / 2,
    };
    let result = inject_click(MouseButton::Left, aimed);
    let landed = match cursor_position() {
        Ok(point) => point,
        Err(why) => {
            stop(&mut child);
            panic!("GetCursorPos failed: {why}");
        }
    };
    stop(&mut child);
    let delivery = match result {
        Ok(delivery) => delivery,
        Err(why) => panic!("inject_click to ({},{}) failed: {why}", aimed.x, aimed.y),
    };

    // The scaling is lossy by construction: a 1024-wide desktop gives each pixel
    // 64 units, so the pointer can legitimately sit one pixel off the exact
    // centre of an odd-sized window. Anything larger than that is not rounding.
    let drift_x = (landed.x - aimed.x).abs();
    let drift_y = (landed.y - aimed.y).abs();
    assert!(
        drift_x <= 1 && drift_y <= 1,
        "aimed at ({},{}) and the pointer ended at ({},{}): drift {},{} exceeds one pixel, so \
         the absolute-coordinate scaling does not round-trip on this display ({:?})",
        aimed.x,
        aimed.y,
        landed.x,
        landed.y,
        drift_x,
        drift_y,
        delivery,
    );
    note(&format!(
        "aimed {},{} landed {},{} (drift {},{}) {:?}",
        aimed.x, aimed.y, landed.x, landed.y, drift_x, drift_y, delivery
    ));
}

/// 3. A posted click does not move the human's pointer.
#[test]
#[ignore = "needs a real interactive desktop; run by the artifact workflow with --ignored"]
fn input_dump_a_posted_click_leaves_the_cursor_alone() {
    if !interactive_session() {
        note("no interactive seat on this host, so there is no pointer to protect");
        return;
    }
    let marker = format!("OCIN-POST-{}", std::process::id());
    let mut child = spawn_marker_window(&marker);
    let window = match wait_for_window(&marker) {
        Ok(window) => window,
        Err(why) => {
            stop(&mut child);
            panic!("{why}");
        }
    };
    let before = match cursor_position() {
        Ok(point) => point,
        Err(why) => {
            stop(&mut child);
            panic!("GetCursorPos failed: {why}");
        }
    };
    // Somewhere well away from where the pointer already is, so "it did not move"
    // cannot be satisfied by coincidence.
    let aimed = ScreenPoint {
        x: window.rect.left + 40,
        y: window.rect.top + 60,
    };
    let result = click_window(window.hwnd, MouseButton::Left, aimed);
    let after = match cursor_position() {
        Ok(point) => point,
        Err(why) => {
            stop(&mut child);
            panic!("GetCursorPos failed after the click: {why}");
        }
    };
    let _ = capture_stable(window.hwnd)
        .and_then(|capture| write_png("after-posted-click.png", &capture));
    stop(&mut child);

    let delivery = match result {
        Ok(delivery) => delivery,
        Err(why) => panic!("click_window failed: {why}"),
    };
    assert_eq!(
        before, after,
        "a posted click moved the pointer from {:?} to {:?}, which is exactly what this route \
         exists to avoid",
        before, after
    );
    let Delivery::Queued { messages } = delivery else {
        panic!("click_window must report a queued delivery, got {delivery:?}");
    };
    assert_eq!(messages, 3, "move, press, release");
    note(&format!(
        "posted click to {} at {},{} left the pointer at {:?} ({messages} messages queued)",
        window.hwnd, aimed.x, aimed.y, before
    ));
}
