#![cfg(windows)]
//! A keystroke receipt that does not need the camera.
//!
//! The frames in `desktop_input_dump_test` prove a lot, but they prove it through
//! the capture path, and a broken capture path is indistinguishable from a broken
//! input path when the only instrument is a frame comparison: both come back as
//! "nothing changed". That is not hypothetical. The first run of those jobs came
//! back exactly like that, and what it was actually reporting was a test window
//! with no process reading its input.
//!
//! So this job closes the loop with a different sensor. Type a command that
//! changes the window's own title, press Enter, and read the title back from the
//! window list (`GetWindowTextW`, the same enumeration the tool surface returns).
//! A title is a string the shell put in a kernel object because it read a line we
//! typed, which separates the three outcomes a frame-only test cannot:
//!
//! - title moved and the frame moved: the lane works and the capture agrees.
//! - title moved and the frame did not: the input lane works and the capture is
//!   the liar. Worth knowing, and no frame comparison can tell it apart from the
//!   line above, because both are "flat".
//! - title did not move: the keystrokes never reached the shell, whatever the
//!   pixels say.
//!
//! The payload is `title`, sent to a console window this job opened itself, and it
//! changes that window's caption and nothing else: no file is written, no path is
//! created, no network is touched, no other application is addressed. That narrow
//! a blast radius is why Enter is allowed here at all, when the frame jobs
//! deliberately never press it.

use crate::desktop::Key;
use crate::desktop::{Delivery, inject_key, inject_text, interactive_session, list_windows};
use crate::tests::desktop_input_util::{
    capture_stable, close_window, note, spawn_marker_window, stop, wait_for_title, wait_for_window,
    write_png,
};

/// The typed command, with the pid in the new title so two runs cannot read each
/// other's result.
fn done_marker() -> String {
    format!("OCIN-DONE-{}", std::process::id())
}

/// 5. Characters we injected make the shell rename its own window.
#[test]
#[ignore = "needs a real interactive desktop; run by the artifact workflow with --ignored"]
fn input_dump_a_typed_command_changes_the_window_title() {
    if !interactive_session() {
        note("no interactive seat on this host, so there is no shell to type into");
        return;
    }
    let marker = format!("OCIN-EXEC-{}", std::process::id());
    let mut child = spawn_marker_window(&marker);
    let window = match wait_for_window(&marker) {
        Ok(window) => window,
        Err(why) => {
            stop(&mut child);
            panic!("{why}");
        }
    };

    // Same refusal as the frame jobs: injected input goes to the focused window,
    // and a window that lost focus in the meantime would turn this into typing a
    // command into someone else's application. Here that refusal is not only
    // politeness, because this job presses Enter.
    if !window.foreground {
        stop(&mut child);
        panic!(
            "the marker window lost focus before it could be typed into; this job sends Enter, so \
             nothing was injected anywhere"
        );
    }

    let before = match capture_stable(window.hwnd) {
        Ok(capture) => capture,
        Err(why) => {
            stop(&mut child);
            panic!("{why} before typing a command");
        }
    };

    // capture_stable takes seconds, and focus can move in that time. The guard
    // above ran before it, so read the foreground again now: this job presses
    // Enter, and the failure the guard exists to prevent is typing into
    // whatever gained focus while the frame was being taken.
    let foreground = match list_windows() {
        Ok(list) => list.foreground().map(|w| w.hwnd),
        Err(why) => {
            stop(&mut child);
            panic!("{why} while re-reading the foreground window");
        }
    };
    if foreground != Some(window.hwnd) {
        stop(&mut child);
        panic!(
            "focus moved off the marker window while it was being photographed; this job sends \
             Enter, so nothing was injected anywhere"
        );
    }

    let command = format!("title {}", done_marker());
    let typed = match inject_text(&command) {
        Ok(delivery) => delivery,
        Err(why) => {
            stop(&mut child);
            panic!("inject_text failed: {why}");
        }
    };
    let enter = match inject_key(Key::Enter) {
        Ok(delivery) => delivery,
        Err(why) => {
            stop(&mut child);
            panic!("inject_key(Enter) failed: {why}");
        }
    };

    // The claim, read back without a single pixel: the shell saw the line, ran it,
    // and renamed the window we are watching. Addressed by handle, because from
    // here on the marker title no longer exists.
    let title = match wait_for_title(window.hwnd, &done_marker()) {
        Ok(title) => title,
        Err(why) => {
            let _ = write_png("exec-before.png", &before);
            stop(&mut child);
            panic!("{why}");
        }
    };
    let after = match capture_stable(window.hwnd) {
        Ok(capture) => capture,
        Err(why) => {
            stop(&mut child);
            panic!("{why} after the shell renamed its window");
        }
    };
    let _ = write_png("exec-before.png", &before);
    let _ = write_png("exec-after.png", &after);
    // The console we drove has earned its close, and this is the one path that
    // reaches it: close_window is the polite request, and stop() takes back the
    // launcher (already gone). Panic paths above skip this on purpose, which is
    // why the marker title carries this process's pid.
    close_window(window.hwnd);
    stop(&mut child);

    let Delivery::Injected { events } = typed else {
        panic!("inject_text must report an injected delivery");
    };
    let Delivery::Injected {
        events: enter_events,
    } = enter
    else {
        panic!("inject_key must report an injected delivery");
    };
    assert_eq!(
        events,
        command.chars().count() * 2,
        "{} characters in {command:?} are {} press/release pairs",
        command.chars().count(),
        events
    );
    assert_eq!(
        enter_events, 2,
        "Enter is one press and one release, and {enter_events} events is neither"
    );

    // Deliberately `> 0` rather than a threshold. The title already proved the
    // input arrived; this assertion is only about the camera, and the finding it
    // produces is a capture finding: a window whose caption the shell itself
    // changed, photographed byte-for-byte identical, is a capture that is not
    // showing what the window paints.
    let moved = before
        .changed_pixels(&after)
        .expect("the same window photographed at the same size");
    assert!(
        moved > 0,
        "the shell renamed its own window to {title:?} and the caption did not move by a single \
         pixel: the keystrokes demonstrably reached the shell, so this is the capture lying, not \
         the input lane. Fix the sensor, not the sender.",
    );
    note(&format!(
        "typed {command:?} + Enter: {events} + {enter_events} events, title is now {title:?}, \
         {moved} pixels moved in the frame"
    ));
}
