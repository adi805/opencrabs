#![cfg(windows)]
//! Shared machinery for the receipts that drive a real Windows desktop.
//!
//! Split out of `desktop_input_dump_test` because the same three questions come
//! up in every one of them: did a window of ours appear, is it still the one in
//! the foreground, and what did its frame do after the injection. Copying that
//! per file is how one copy drifts, and a drift here changes what a green run
//! means, which is the only thing these files are for.
//!
//! Nothing in this module asserts anything. It opens a window, finds it,
//! photographs it, and reports. The claims live with the tests.

use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use crate::desktop::{WindowInfo, capture_window, list_windows};

/// `CREATE_NEW_CONSOLE`: without it `cmd.exe` inherits this process's console and
/// there is no separate window to drive.
pub(crate) const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;

/// How long to wait for the console to appear, or for a title to change.
pub(crate) const WINDOW_TIMEOUT: Duration = Duration::from_secs(20);

/// A short pause so conhost has repainted before the second photograph.
///
/// Not a synchronisation primitive: Windows gives no way to ask a window
/// whether it has finished painting, so the honest options are a delay or a
/// retry on the measurement itself. This is the delay, and it only sets a
/// floor; the receipts retry on the measurement (see [`capture_stable`]).
pub(crate) const PAINT_SETTLE: Duration = Duration::from_millis(250);

/// Where the frames land, so the artifact workflow can hand them back.
pub(crate) fn out_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("input-dump");
    std::fs::create_dir_all(&dir).expect("create target/input-dump");
    dir
}

/// One line of narration per measurement, so a red run can be read without
/// downloading the frames.
pub(crate) fn note(line: &str) {
    println!("[input-dump] {line}");
}

/// Open a console that prints its marker and then sits at its own prompt.
///
/// The prompt is the point, and it cost one red run to learn it. The first
/// version of this helper kept the window alive with `ping -n 120 127.0.0.1`,
/// which is a *running program*: with nothing reading the console, Windows
/// parks typed characters in conhost's typeahead buffer and paints none of
/// them, so both typing receipts compared two frames of a window that could
/// not change and blamed the input lane. `cmd /K` with builtins only leaves the
/// shell in line-input mode with echo on, which is the state where an injected
/// character reaches the screen buffer at all.
///
/// No receipt here sends Enter to that prompt unless it says so in its own
/// name, so a stray character never becomes a command by accident.
pub(crate) fn spawn_marker_window(marker: &str) -> Child {
    let script = format!("title {marker} & mode con cols=100 lines=20 & echo {marker}");
    Command::new("cmd.exe")
        .args(["/K", script.as_str()])
        .creation_flags(CREATE_NEW_CONSOLE)
        .spawn()
        .expect("spawn cmd.exe in a new console")
}

/// Kill the console this job opened. Windows does not clean up a child spawned
/// with `CREATE_NEW_CONSOLE` when the parent panics, so every exit path here
/// goes through this.
pub(crate) fn stop(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Find the window this job opened, by the title only it carries.
///
/// Polling instead of a single read: the window does not exist the instant
/// `spawn` returns, and "the console never appeared" is a different finding
/// from "the title was not what we asked for".
pub(crate) fn wait_for_window(marker: &str) -> Result<WindowInfo, String> {
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

/// The title a window carries right now, addressed by handle rather than by
/// marker, so a caller can watch it change.
pub(crate) fn title_of(hwnd: isize) -> Option<String> {
    let list = list_windows().ok()?;
    list.windows
        .iter()
        .find(|w| w.hwnd == hwnd)
        .map(|w| w.title.clone())
}

/// Wait until the window's title contains `wanted`.
///
/// This is the receipt that needs no camera: a title is a string the shell put
/// in a kernel object because it read a line we typed. Nothing about it depends
/// on painting, on the capture path, or on how much light a glyph happens to
/// carry, which is why the typing tests have this as their stronger half.
pub(crate) fn wait_for_title(hwnd: isize, wanted: &str) -> Result<String, String> {
    let deadline = Instant::now() + WINDOW_TIMEOUT;
    let mut last = String::from("(unreadable)");
    while Instant::now() < deadline {
        match title_of(hwnd) {
            Some(title) if title.contains(wanted) => return Ok(title),
            Some(title) => last = title,
            None => last = String::from("(window gone)"),
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Err(format!(
        "title never contained *{wanted}* within {}s; last read {last:?} \
         (a title that did not move means the shell never read the line we typed)",
        WINDOW_TIMEOUT.as_secs()
    ))
}

/// Capture until the frame carries any ink at all, or give up after five seconds.
pub(crate) fn capture_stable(hwnd: isize) -> Result<crate::desktop::Capture, String> {
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

/// How many pixels `ink_ratio` counted as lit in this frame.
///
/// A ratio is the right thing to compare across window sizes and the wrong
/// thing to set a floor against: 0.0001 of a 1044x635 console is 66 pixels,
/// which one inverse-video caret cell clears on its own. Claims about "a real
/// change happened" are made against pixels for that reason.
pub(crate) fn lit_pixels(capture: &crate::desktop::Capture) -> i64 {
    let total = i64::from(capture.width) * i64::from(capture.height);
    (capture.ink_ratio * total as f64).round() as i64
}

/// Write a frame to the artifact directory and narrate it.
pub(crate) fn write_png(name: &str, capture: &crate::desktop::Capture) -> Result<(), String> {
    let path = out_dir().join(name);
    let png = capture
        .to_png()
        .map_err(|e| format!("encode {name}: {e}"))?;
    std::fs::write(&path, png).map_err(|e| format!("write {}: {e}", path.display()))?;
    note(&format!(
        "{name}: {}x{} ink={:.6} ({} lit px)",
        capture.width,
        capture.height,
        capture.ink_ratio,
        lit_pixels(capture)
    ));
    Ok(())
}
