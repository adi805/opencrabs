#![cfg(windows)]
//! App-control receipts, run on the Windows runner that has a real desktop.
//!
//! The input lane proves a pointer moves and a keystroke reaches a console. These
//! prove the three app actions against a window the operating system made for a
//! program this crate started: that the window is attributed to the child rather
//! than assumed, that asking for the foreground is confirmed by reading it back
//! instead of by the call's own return value, and that asking a window to close is
//! measured by the window ceasing to exist.
//!
//! Each one is `#[ignore]`d. A headless job cannot produce a window, so an
//! unignored version would be a test that passes by never being attempted.
//! `windows-artifact.yml` runs them with `--ignored` on a runner that has a
//! session, and the PNG count gates that step.

use std::process;

use crate::desktop;
use crate::desktop::app::{self, CloseVerdict, FocusVerdict, LaunchPlan};
use crate::tests::desktop_input_util::{close_window, lit_pixels, note, out_dir, write_png};

/// Programs to try, in order.
///
/// `notepad.exe` is the expected one: it owns its window outright rather than
/// handing it to the shared console host that every console window on this
/// runner is merged into, and it opens with nothing on screen to disagree about.
/// The others are here so that one absent app on a different image is not read
/// as a broken lane. Every failure is printed, so a run that had to fall through
/// to a later candidate still says so.
const CANDIDATES: &[&str] = &["notepad.exe", "mspaint.exe", "charmap.exe"];

fn launch_and_find(name: &str) -> Result<(u32, desktop::WindowInfo), String> {
    let plan = LaunchPlan::build(name, &[]).map_err(|why| format!("{name}: {why}"))?;
    let (pid, found) = desktop::launch_to_window(&plan, app::LAUNCH_WINDOW_SETTLE)
        .map_err(|why| format!("{name}: {why}"))?;
    match found {
        Some(window) => {
            note(&format!(
                "launched {name} as pid {pid}, window {}",
                window.hwnd
            ));
            Ok((pid, window))
        }
        None => Err(format!(
            "{name}: started as pid {pid} and showed no window within {:?}",
            app::LAUNCH_WINDOW_SETTLE
        )),
    }
}

/// Every candidate tried, each failure kept. A receipt that quietly used a
/// different program than the one that failed would hide the failure.
fn first_window_around() -> Result<(u32, desktop::WindowInfo), String> {
    let mut failures = Vec::new();
    for name in CANDIDATES {
        match launch_and_find(name) {
            Ok((pid, window)) => return Ok((pid, window)),
            Err(why) => {
                note(&format!("candidate {name} gave no window: {why}"));
                failures.push(why);
            }
        }
    }
    Err(format!(
        "no candidate produced a window to act on: {}",
        failures.join(" | ")
    ))
}

/// Whether the desktop still lists this handle, asked of the enumeration rather
/// than of `IsWindow`.
///
/// `close_target` polls `IsWindow` internally, so a receipt that confirmed its
/// answer with the same call would only be re-reading one witness. The
/// enumeration is the second one, and it is the same view an agent calling
/// `list_windows` would get afterwards, which is what makes a "Gone" verdict
/// worth something to whoever reads the log.
fn still_listed(hwnd: isize) -> bool {
    match desktop::list_windows() {
        Ok(list) => list.windows.iter().any(|window| window.hwnd == hwnd),
        Err(why) => {
            note(&format!(
                "could not enumerate windows to check {hwnd}: {why}"
            ));
            true
        }
    }
}

/// Best effort between receipts: a leaked window makes the next one harder to
/// read, and a failed cleanup is not a failed receipt.
fn tidy(launched_pid: u32, window: &desktop::WindowInfo) {
    let hwnd = window.hwnd;
    if still_listed(hwnd) {
        note(&format!(
            "tidy-up: window {hwnd} is still listed, asking it to close"
        ));
        close_window(hwnd);
    }
    // A request is not a guarantee. `WM_CLOSE` is something a program can refuse:
    // `mspaint.exe` answers with a save dialog and the window stays, and a leaked
    // window occupies the desktop for every later receipt in the same job. The
    // sweep is scoped to the pid THIS test started, never to `window.pid`: the
    // owner of a console window is the shared console host, and `taskkill /F /T`
    // on that would take the job's own terminal down with it. `app::close_target_allowed`
    // refuses the same classes for the same reason, and a test that panicked
    // before reaching here is not cleaned, because the runner dies with it.
    if launched_pid != process::id() && window.pid == launched_pid {
        note(&format!(
            "tidy-up: sweeping pid {launched_pid} and its children"
        ));
        crate::utils::shell::kill_process_tree(launched_pid);
    }
}

/// Captures, writes the frame, and reports its size and how much of it is lit,
/// so the file the job counts and the number the log prints are the same object.
fn evidence(name: &str, window: &desktop::WindowInfo) -> Result<(), String> {
    let capture = desktop::capture_window(window.hwnd)
        .map_err(|why| format!("capture {}: {why}", window.hwnd))?;
    let lit = lit_pixels(&capture);
    write_png(name, &capture)?;
    let bytes = std::fs::read(out_dir().join(name)).unwrap_or_default();
    note(&format!(
        "{name}: {}x{}, window {}, {} bytes on disk, {lit} lit pixels",
        capture.width,
        capture.height,
        window.hwnd,
        bytes.len()
    ));
    if bytes.is_empty() {
        return Err(format!("{name} was written and read back empty"));
    }
    if lit == 0 {
        return Err(format!("{name} is entirely unlit: nothing was painted"));
    }
    Ok(())
}

#[test]
#[ignore = "needs a Windows session with a desktop; runs in windows-artifact.yml"]
fn app_dump_a_launched_program_shows_a_window_we_can_attribute_to_it() {
    let (pid, window) = first_window_around().expect("a launched program should show a window");
    assert_ne!(
        window.hwnd, 0,
        "handle 0 is the value for \"no window\", not a window to act on"
    );
    assert_ne!(
        window.pid, 0,
        "pid 0 is the value for \"owner unreadable\", and the claim here is that the window \
         belongs to the process we started"
    );
    assert_eq!(
        window.pid, pid,
        "the enumeration attributes window {} to pid {}, but we started the program as pid {} \
         and the whole claim of this receipt is that pair",
        window.hwnd, window.pid, pid
    );
    evidence("app-window.png", &window).expect("write app-window.png");
    tidy(pid, &window);
}

#[test]
#[ignore = "needs a Windows session with a desktop; runs in windows-artifact.yml"]
fn app_dump_focus_is_read_back_from_the_desktop_not_from_the_call() {
    let (pid, window) = first_window_around().expect("a launched program should show a window");
    let verdict = desktop::focus_target(window.hwnd)
        .expect("the foreground window should be readable on a session that has one");
    note(&format!(
        "focus_window({}) answered: {verdict}",
        window.hwnd
    ));
    assert!(
        matches!(verdict, FocusVerdict::Confirmed { .. }),
        "asked for window {} and the desktop left something else in front: {verdict}. The \
         foreground lock is allowed to refuse us, and the honest record of that is this \
         failure, not the return value of the call.",
        window.hwnd
    );
    evidence("app-focus.png", &window).expect("write app-focus.png");
    tidy(pid, &window);
}

#[test]
#[ignore = "needs a Windows session with a desktop; runs in windows-artifact.yml"]
fn app_dump_closing_a_window_is_measured_by_it_stopping_to_exist() {
    let (pid, window) = first_window_around().expect("a launched program should show a window");
    let hwnd = window.hwnd;
    let before = still_listed(hwnd);
    let verdict = desktop::close_target(&window, process::id(), app::CLOSE_SETTLE)
        .expect("closing a program we started ourselves must not be refused");
    note(&format!(
        "close verdict for window {hwnd}, launched as pid {pid}: {verdict}"
    ));
    let after = still_listed(hwnd);
    assert!(
        matches!(verdict, CloseVerdict::Gone { .. }),
        "asked window {hwnd} to close and got {verdict}. The enumeration listed it {before} \
         before and {after} after: a window still enumerable after the request is an \
         application that has not gone, whatever the post itself returned.",
    );
    assert!(
        !after,
        "the verdict said Gone but the enumeration still lists {hwnd} as a window"
    );
    match desktop::capture_window(hwnd) {
        Err(why) => note(&format!(
            "capturing the closed window refuses, as it should: {why}"
        )),
        Ok(_) => panic!("window {hwnd} captured after it was reported Gone"),
    }
}
