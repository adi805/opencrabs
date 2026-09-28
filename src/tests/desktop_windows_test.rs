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

use crate::desktop::{MAX_WINDOWS, interactive_session, is_shell_backdrop, list_windows};

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
