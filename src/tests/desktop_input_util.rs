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
/// The prompt is the point, and it cost two red runs to learn how to get one.
///
/// First attempt: keep the window alive with `ping -n 120 127.0.0.1`. That is a
/// *running program*, so nothing reads the console, and Windows parks typed
/// characters in the typeahead buffer and paints none of them. Both typing
/// receipts compared two frames of a window that could not change, and blamed
/// the input lane.
///
/// Second attempt: drop the `ping` and let `cmd /K` reach its prompt. Measured
/// by `desktop_input_probe_test` on the runner, that console's process was
/// already `EXITED` within a hundred milliseconds, and a third variant with
/// null standard handles never produced a window at all. The reason is that our
/// stdin is the job's, which is a pipe at end-of-file: a shell reading its
/// prompt from a dead pipe gets EOF and quits. Two different-looking failures,
/// one cause, and the capture lane was innocent both times.
///
/// So we let `cmd` launch the console instead of spawning it ourselves. `start`
/// creates the process without handing it our standard handles, which means its
/// stdin is the input buffer of its own console: the prompt waits for keyboard
/// input, an injected character is echoed by the shell, and Enter submits a
/// line. The `Child` returned here is the launcher, which exits at once and is
/// no longer the thing keeping the window open.
///
/// That has one cost we accept knowingly: nothing kills the console when a test
/// panics, so a red run leaves its windows on screen until the job ends. The
/// runner is disposable and every marker title carries this process's id, so a
/// leftover cannot be mistaken for another test's window. `close_window` exists
/// for the paths that do reach their end.
pub(crate) fn spawn_marker_window(marker: &str) -> Child {
    // `start`'s grammar is `start ["title"] program [args...]`, decided by
    // position: a bare word in the title slot is read as the program to run, so
    // this line used to ask Windows for a program named "opencrabs-input-probe",
    // which does not exist. Windows answered with a #32770 error dialog,
    // `cmd.exe` was never started, and every receipt failed in wait_for_window
    // against a window nobody had opened. The title has to be quoted to be a
    // title; that one character pair is the difference between a console and an
    // error box.
    let script = format!("title {marker} & mode con cols=100 lines=20 & echo {marker}");
    Command::new("cmd.exe")
        .args([
            "/C",
            "start",
            "\"opencrabs-input-probe\"",
            "cmd.exe",
            "/K",
            script.as_str(),
        ])
        .creation_flags(CREATE_NEW_CONSOLE)
        .spawn()
        .expect("spawn cmd.exe through start, in its own console")
}

/// Ask a console to close itself by handle, the polite route.
///
/// Not a process kill: see [`crate::desktop::WM_CLOSE`] for why the pid on a
/// console window belongs to the host and must never be terminated from here.
pub(crate) fn close_window(hwnd: isize) {
    let posted = crate::desktop::post_close(hwnd);
    note(&format!(
        "closed window {hwnd}: {}",
        if posted { "posted WM_CLOSE" } else { "refused" }
    ));
}

/// Every top-level window, as one line each, for the diagnostic output.
pub(crate) fn window_table() -> String {
    match crate::desktop::list_windows() {
        Ok(list) => list
            .windows
            .iter()
            .map(|w| {
                format!(
                    "    hwnd={} pid={} {}x{} +{},{} [{}] {:?}",
                    w.hwnd,
                    w.pid,
                    w.rect.width(),
                    w.rect.height(),
                    w.rect.left,
                    w.rect.top,
                    w.class,
                    w.title
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Err(why) => format!("    (enumeration failed: {why})"),
    }
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
