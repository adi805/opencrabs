//! Tests for the desktop module's selection rules and snapshot shape.
//!
//! These are deliberately cross-platform: the rules decide what an agent gets
//! to see and act on, and they are the part that can be wrong without anyone
//! noticing. They run in the Linux test job and on any host that compiles the
//! pure module.

use crate::desktop::{
    MAX_WINDOWS, Rect, WindowInfo, WindowList, is_shell_backdrop, keep_candidate,
};

fn rect(left: i32, top: i32, right: i32, bottom: i32) -> Rect {
    Rect {
        left,
        top,
        right,
        bottom,
    }
}

fn window(hwnd: isize, class: &str, title: &str, area_rect: Rect) -> WindowInfo {
    WindowInfo {
        hwnd,
        pid: 4242,
        title: title.to_string(),
        class: class.to_string(),
        rect: area_rect,
        foreground: false,
    }
}

#[test]
fn area_is_arithmetic_and_never_a_validity_test() {
    assert_eq!(rect(0, 0, 800, 600).width(), 800);
    assert_eq!(rect(0, 0, 800, 600).height(), 600);
    assert_eq!(rect(0, 0, 800, 600).area(), 480_000);

    // Negative origins are normal (a monitor up and to the left) and real.
    assert_eq!(rect(-1920, -1080, -1120, -480).area(), 480_000);
    assert!(rect(-1920, -1080, -1120, -480).has_positive_span());

    // The case an `area() > 0` test cannot catch, and the reason the rule uses
    // `has_positive_span`: both edges inverted multiplies out positive, so a
    // nonsense rect looks exactly like an 800x600 window by product alone.
    let inverted = rect(800, 600, 0, 0);
    assert_eq!(inverted.area(), 480_000, "the product is positive");
    assert!(
        !inverted.has_positive_span(),
        "and it is still not a window that could be painted"
    );
    assert!(
        !keep_candidate(true, &inverted, "SomeApp"),
        "the candidate rule must reject what the product alone would admit"
    );
}

#[test]
fn one_inverted_edge_is_caught_by_either_test() {
    let width_flipped = rect(800, 0, 0, 600);
    assert_eq!(width_flipped.width(), -800);
    assert_eq!(width_flipped.area(), -480_000);
    assert!(!width_flipped.has_positive_span());
    assert!(!keep_candidate(true, &width_flipped, "SomeApp"));
}

#[test]
fn desktop_furniture_is_dropped_by_class_not_by_title() {
    for backdrop in ["Progman", "WorkerW", "Shell_TrayWnd"] {
        assert!(
            is_shell_backdrop(backdrop),
            "{backdrop} should be furniture"
        );
        assert!(!keep_candidate(true, &rect(0, 0, 100, 50), backdrop));
    }
    // Class names are matched case-insensitively: the API's casing is not part
    // of the contract, and a missed case here silently re-admits the taskbar.
    assert!(is_shell_backdrop("shell_traywnd"));
    assert!(is_shell_backdrop("WORKERW"));
    assert!(!is_shell_backdrop("Chrome_WidgetWin_1"));
    assert!(!is_shell_backdrop(""));
}

#[test]
fn an_untitled_window_is_still_a_candidate() {
    // The rule must not filter on title. Dialogs before a caption is set,
    // renderer surfaces, and games are exactly the windows worth driving, and
    // dropping them would hide interactive targets behind a cosmetic property.
    assert!(keep_candidate(
        true,
        &rect(0, 0, 1, 1),
        "SomeAppWindowClass"
    ));
    assert!(keep_candidate(
        true,
        &rect(0, 0, 100, 100),
        "SomeAppWindowClass"
    ));
    assert_eq!(
        window(3, "SomeAppWindowClass", "", rect(0, 0, 10, 10)).title,
        "",
        "an empty title is representable, so nothing upstream depends on it"
    );
}

#[test]
fn invisible_and_zero_span_windows_are_never_candidates() {
    assert!(!keep_candidate(
        false,
        &rect(0, 0, 400, 300),
        "SomeAppWindowClass"
    ));
    assert!(!keep_candidate(
        true,
        &rect(0, 0, 0, 300),
        "SomeAppWindowClass"
    ));
    assert!(!keep_candidate(false, &rect(0, 0, 0, 0), "Progman"));
    assert!(window(1, "C", "t", rect(0, 0, 0, 0)).is_degenerate());
    assert!(
        !window(1, "C", "t", rect(0, 0, 640, 480)).is_degenerate(),
        "a normal window must not be called degenerate"
    );
}

#[test]
fn window_list_reports_foreground_emptiness_and_length() {
    let mut list = WindowList {
        windows: vec![
            window(10, "Notepad", "notes.txt", rect(0, 0, 100, 50)),
            window(11, "Chrome", "tab", rect(0, 0, 200, 200)),
        ],
        truncated: false,
    };
    assert_eq!(list.len(), 2);
    assert!(!list.is_empty());
    assert!(list.foreground().is_none(), "no window claims focus");

    list.windows[1].foreground = true;
    assert_eq!(list.foreground().map(|w| w.hwnd), Some(11));

    assert!(
        WindowList {
            windows: Vec::new(),
            truncated: false,
        }
        .is_empty()
    );
}

#[test]
fn a_truncated_snapshot_says_so_in_its_rendered_form() {
    // The point of the flag is that a reader of the text cannot mistake a short
    // list for a complete one, so the note has to be in the output, not only in
    // the struct.
    let complete = WindowList {
        windows: vec![window(1, "Notepad", "a.txt", rect(0, 0, 10, 10))],
        truncated: false,
    };
    let text = complete.to_string();
    assert!(text.contains("Notepad"));
    assert!(text.contains("a.txt"));
    assert!(!text.contains("truncated"));

    let short = WindowList {
        windows: vec![window(2, "Chrome", "b", rect(0, 0, 10, 10))],
        truncated: true,
    };
    assert!(
        short
            .to_string()
            .contains(&format!("truncated at {MAX_WINDOWS}"))
    );
}

#[test]
fn the_cap_is_large_enough_to_be_true_and_small_enough_to_be_bounded() {
    // Not a tautology about the constant's value: the bound only helps if it is
    // below what a real desktop can produce, and the filtering only helps if it
    // is above what an ordinary session shows. Both sides are checked against
    // the documented expectation, so retuning the number is a decision with a
    // test that reacts.
    assert!(
        MAX_WINDOWS >= 64,
        "a cap this low would truncate ordinary sessions"
    );
    assert!(
        MAX_WINDOWS <= 1024,
        "a cap this high defeats the bounded-context reason for the type"
    );
}
