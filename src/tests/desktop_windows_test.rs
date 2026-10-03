#![cfg(windows)]
//! Windows-only behaviour for the desktop backend.
//!
//! These run on a real Windows host, in the CI Windows test slice, and nowhere
//! else: the Linux and macOS jobs compile them to nothing. That matters because
//! everything here reads kernel state, so a Linux run could not confirm any of
//! it even in principle.
//!
//! Deliberately absent: an assertion about the *number* of windows, or about
//! what is on screen. A CI runner may be at an interactive console or in a
//! service context, and either state is correct for the machine, so the only
//! honest assertions are the invariants that must hold in both.

use crate::desktop::{
    MAX_WINDOWS, MIN_INK_RATIO, capture_window, capture_window_to_png, ink_ratio,
    interactive_session, is_shell_backdrop, list_windows,
};

#[test]
fn enumerating_windows_does_not_fail_on_a_headless_or_service_seat() {
    // The claim is specific: an isolated window station still answers the walk
    // successfully, it just has nothing useful in it. If this returns Err, the
    // caller's report is "desktop control is broken" instead of "nothing here",
    // which is the confusion this module exists to prevent.
    let list = list_windows().expect("EnumWindows must not fail on a valid window station");
    // A service seat legitimately shows zero windows, so emptiness is not an
    // error; the type still has to be self-consistent about it.
    assert_eq!(list.is_empty(), list.windows.is_empty());
}

#[test]
fn every_reported_window_passes_the_candidate_rule() {
    // The filter lives in `model::keep_candidate`. This checks the backend
    // actually applied it, rather than returning raw enumeration and letting a
    // caller trust a list that still contains the taskbar and wallpaper.
    let list = list_windows().expect("enumeration must succeed");
    for w in &list.windows {
        assert_ne!(w.hwnd, 0, "a real HWND is never null");
        assert!(
            !w.is_degenerate(),
            "{w:?} has no paintable span and should have been dropped"
        );
        assert!(
            !is_shell_backdrop(&w.class),
            "{} is desktop furniture and should have been dropped",
            w.class
        );
        assert_ne!(w.pid, 0, "GetWindowThreadProcessId must resolve an owner");
    }
}

#[test]
fn at_most_one_window_claims_the_keyboard() {
    let list = list_windows().expect("enumeration must succeed");
    let focused: Vec<_> = list.windows.iter().filter(|w| w.foreground).collect();
    assert!(
        focused.len() <= 1,
        "Windows has one foreground window; found {}",
        focused.len()
    );
    assert_eq!(
        list.foreground().map(|w| w.hwnd),
        focused.first().map(|w| w.hwnd)
    );
}

#[test]
fn the_snapshot_stays_inside_its_bound_and_says_when_it_does_not() {
    let list = list_windows().expect("enumeration must succeed");
    assert!(
        list.len() <= MAX_WINDOWS,
        "a snapshot of {} windows exceeded the documented bound of {MAX_WINDOWS}",
        list.len()
    );
    // The two halves of the truncation contract: hitting the cap implies the
    // flag, and a list below the cap must not claim to be cut short.
    if list.len() == MAX_WINDOWS {
        assert!(
            list.truncated,
            "a full-to-the-cap list must be marked truncated"
        );
    } else {
        assert!(!list.truncated, "a short list is not a truncated one");
    }
}

#[test]
fn the_seat_answer_is_stable_within_one_process() {
    // Two readings of the same question must not disagree: the check combines
    // three kernel calls, and a mistake in how their results are combined (for
    // instance treating a null window station as truthy) shows up as an answer
    // that changes between calls.
    let first = interactive_session();
    let second = interactive_session();
    assert_eq!(first, second, "the seat answer flipped inside one process");
}

#[test]
fn a_null_handle_is_refused_as_input_not_reported_as_blank() {
    // The two answers mean different things and must not be confused. "You gave
    // me 0" is a caller bug (`GetForegroundWindow` uses 0 for "nothing", so a
    // caller that skipped that check would land here); "the frame is blank" is a
    // window that did not paint. Reporting the first as the second sends whoever
    // debugs it looking at the wrong end of the pipe.
    let error = capture_window(0).expect_err("0 is not a window");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert!(
        error.to_string().contains("not a window handle"),
        "unhelpful message: {error}"
    );

    let error = capture_window_to_png(0, &std::path::Path::new("/dev/null"))
        .expect_err("0 is not a window");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn a_handle_that_is_not_a_window_fails_with_a_reason() {
    // A made-up handle must produce an error carrying a reason, never a frame.
    // The point is that the failure path is loud: a silent empty capture here is
    // how an agent ends up reasoning about a screenshot of nothing.
    let error = capture_window(0x1234_5678).expect_err("not a real window");
    assert!(
        !error.to_string().is_empty(),
        "the failure has to say something"
    );
}

#[test]
fn every_listed_window_is_either_photographed_or_told_apart_from_a_blank() {
    // The invariant a caller depends on, and the only one that holds on both an
    // interactive seat and a service one: capture never silently hands back a
    // blank frame. Either it succeeds, or it fails with a reason. A window that
    // genuinely paints nothing (a suspended UWP surface, say) is allowed to fail;
    // what is not allowed is success with an empty image.
    let list = list_windows().expect("enumeration must succeed");
    println!(
        "[desktop] examining {} listed windows on this seat",
        list.len()
    );

    let mut captured = 0usize;
    for window in &list.windows {
        match capture_window(window.hwnd) {
            Ok(capture) => {
                captured += 1;
                assert_eq!(
                    (capture.width, capture.height),
                    (window.rect.width() as u32, window.rect.height() as u32),
                    "capture size disagrees with the rect the list reported for {:?}",
                    window.title
                );
                assert_eq!(
                    capture.rgba.len(),
                    (capture.width as usize) * (capture.height as usize) * 4,
                    "buffer does not match its own dimensions for {:?}",
                    window.title
                );
                // The verdict is recomputed from the pixels rather than trusted,
                // so a `Capture` constructed with a stale ratio is caught here.
                assert!(
                    (capture.ink_ratio - ink_ratio(&capture.rgba)).abs() < f64::EPSILON,
                    "ink ratio is not the ratio of the pixels it carries"
                );
            }
            Err(error) => {
                assert!(
                    !error.to_string().is_empty(),
                    "capture of {:?} failed without saying why",
                    window.title
                );
            }
        }
    }
    println!(
        "[desktop] {captured} of {} windows photographed",
        list.len()
    );
}

#[test]
fn a_successful_capture_to_png_is_never_a_blank_file() {
    // `capture_window_to_png` is the function whose contract matters most: it is
    // the one that claims "this is what the window looks like". `PrintWindow`
    // returns TRUE for windows it failed to paint, so the write succeeding is not
    // evidence; the ink measurement is. This asserts the wiring end to end, on a
    // real window, on a real desktop.
    let list = list_windows().expect("enumeration must succeed");
    let Some(window) = list
        .windows
        .iter()
        .find(|w| !w.is_degenerate() && w.rect.width() > 8 && w.rect.height() > 8)
    else {
        // Legitimate on a service seat. Printed rather than asserted away, so a
        // vacuous run is visible in the log instead of looking like a pass.
        println!("[desktop] no listed window large enough to photograph on this seat");
        return;
    };

    let path = std::env::temp_dir().join(format!("oc-capture-{}.png", window.hwnd));
    let result = capture_window_to_png(window.hwnd, &path);
    match result {
        Ok(capture) => {
            assert!(
                !capture.is_blank(),
                "capture reported success on a blank frame"
            );
            assert!(
                capture.ink_ratio >= MIN_INK_RATIO,
                "success implies the frame cleared the blank threshold, measured {}",
                capture.ink_ratio
            );
            let bytes = std::fs::read(&path).expect("the PNG was written before the check");
            assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "not a PNG on disk");
            let decoded = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
                .expect("the written file decodes")
                .to_rgba8();
            assert_eq!(
                (decoded.width(), decoded.height()),
                (capture.width, capture.height),
                "the file on disk is not the size the capture reports"
            );
            println!(
                "[desktop] photographed {:?} {}x{} ink={:.4}",
                window.title, capture.width, capture.height, capture.ink_ratio
            );
        }
        Err(error) => {
            // Two outcomes wear the same Err. A window that cannot paint at all
            // leaves nothing behind. A BLANK capture is different: the frame is
            // written before the error on purpose, so the evidence survives for
            // whoever debugs it, and the file is expected. Asserting "no file"
            // for both would fail the run on exactly the behaviour the write
            // order exists to produce.
            let message = error.to_string();
            if message.contains("captured blank") {
                assert!(
                    path.exists(),
                    "a blank capture must leave its frame behind for inspection: {message}"
                );
            } else {
                assert!(
                    !path.exists(),
                    "capture failed but left a file behind: {message}"
                );
            }
            println!(
                "[desktop] {:?} could not be photographed: {message}",
                window.title
            );
        }
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_capture_that_cannot_be_written_says_where_it_tried() {
    // The write happens after the pixels are in hand, so this also proves the
    // capture itself succeeded before the failure: a path error that arrives
    // instead of a capture error means the window really did paint. The message
    // has to carry the path, because "cannot write" without one sends the reader
    // looking for a permission problem in the wrong directory.
    let list = list_windows().expect("enumeration must succeed");
    let Some(window) = list
        .windows
        .iter()
        .find(|w| !w.is_degenerate() && w.rect.width() > 8 && w.rect.height() > 8)
    else {
        println!("[desktop] no capturable window on this seat to exercise the write path");
        return;
    };

    let missing = std::env::temp_dir()
        .join(format!("oc-capture-absent-{}", std::process::id()))
        .join("nested")
        .join("never.png");
    assert!(
        !missing.parent().expect("has a parent").exists(),
        "this test needs a directory that does not exist"
    );

    let error = capture_window_to_png(window.hwnd, &missing).expect_err("nothing to write into");
    assert_eq!(
        error.kind(),
        std::io::ErrorKind::NotFound,
        "a missing directory is NotFound, not a capture failure: {error}"
    );
    assert!(
        error.to_string().contains("never.png"),
        "the error must name the path it could not write: {error}"
    );
}

#[test]
fn the_geometry_refusal_is_unreachable_through_the_list() {
    // `capture_window` refuses a rect with no paintable span. That is a second
    // line of defence, not the first: `keep_candidate` already drops such
    // windows from the list, so a caller iterating `list_windows()` can never
    // reach the refusal. Asserting the composition keeps both halves honest --
    // if the filter ever loosens, this fails instead of a capture failing in
    // production for a reason nobody predicted.
    let list = list_windows().expect("enumeration must succeed");
    for window in &list.windows {
        assert!(
            !window.is_degenerate(),
            "the list offered a window with no paintable span: {window:?}"
        );
    }
}
