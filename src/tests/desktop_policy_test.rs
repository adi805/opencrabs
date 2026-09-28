//! The approval rule for desktop actions, and the arguments each one accepts.
//!
//! These run in the ordinary Linux test job, which is the point: the gate that
//! decides whether a human is interrupted before an agent moves their mouse is
//! the part that must not be wrong, and a gate that only compiles on Windows can
//! only ever be exercised on the machine where a mistake is expensive.
//!
//! Nothing here needs a desktop, a window, or a session. `policy` is pure data
//! and pure logic, deliberately kept away from the syscalls so that this file can
//! exist at all.

use crate::desktop::policy::{self, MAX_TYPE_CHARS};
use serde_json::{Value, json};

/// The four names that only look.
const FREE: &[&str] = &[
    "list_windows",
    "capture_window",
    "seat_report",
    "describe_desktop",
];

/// The names that change the machine under whoever is sitting there.
const GATED: &[&str] = &[
    "click_window",
    "type_text",
    "press_key",
    "launch_app",
    "focus_window",
    "close_window",
];

#[test]
fn looking_does_not_need_an_approval_and_touching_does() {
    for action in FREE {
        assert!(
            !policy::requires_approval(action),
            "{action} only reads, so it must not interrupt a human"
        );
    }
    for action in GATED {
        assert!(
            policy::requires_approval(action),
            "{action} changes the desktop, so it must ask first"
        );
    }
}

#[test]
fn a_name_this_tool_does_not_know_still_asks_first() {
    // The direction of this default is the whole safety argument. A future
    // action added to the Windows half and not to the list above must land on
    // the asking side, never the free one.
    assert!(policy::requires_approval("format_the_drive"));
    assert!(policy::requires_approval(""));
}

#[test]
fn every_free_name_is_a_name_the_tool_actually_has() {
    // Two lists that can drift are two lists that will: this pins them together.
    for action in FREE.iter().chain(GATED.iter()) {
        assert!(
            policy::KNOWN_ACTIONS.contains(action),
            "{action} is gated or free but is not in KNOWN_ACTIONS, so the tool would reject it"
        );
    }
    assert_eq!(
        FREE.len() + GATED.len(),
        policy::KNOWN_ACTIONS.len(),
        "a known action is neither in FREE nor GATED, which means nobody decided its rule"
    );
}

#[test]
fn the_action_is_read_out_of_the_input_and_never_defaulted() {
    assert!(policy::action_of(&json!({})).is_err());
    assert!(policy::action_of(&json!({"action": 7})).is_err());
    assert_eq!(
        policy::action_of(&json!({"action": "list_windows"})).unwrap(),
        "list_windows"
    );
}

#[test]
fn an_unknown_name_fails_as_unknown_rather_than_as_unapproved() {
    // The two messages mean different things to the human reading them. "This
    // tool cannot do that" is a full answer; "approve this?" invites someone to
    // approve a typo.
    let err = policy::action_of(&json!({"action": "sudo"})).unwrap_err();
    assert!(err.contains("unknown action"), "got {err:?}");
    assert!(
        err.contains("list_windows"),
        "a rejection should say what is on offer"
    );
}

#[test]
fn typing_refuses_the_shapes_that_would_mean_something_else() {
    let empty = policy::check_input(&json!({"action": "type_text", "text": ""})).unwrap_err();
    assert!(empty.contains("empty"), "got {empty:?}");

    // A newline submits whatever came before it. That is a decision about
    // someone's machine and has to be its own call, not a side effect of a
    // character hidden in a string.
    for sneaky in ["ok\nrm -rf /", "ok\r", "a\nb"] {
        let err = policy::check_input(&json!({"action": "type_text", "text": sneaky})).unwrap_err();
        assert!(err.contains("newline"), "got {err:?} for {sneaky:?}");
    }
}

#[test]
fn typing_a_very_long_string_is_refused_by_count_not_by_bytes() {
    let at_limit = "a".repeat(MAX_TYPE_CHARS);
    // hwnd is supplied because type_text targets a window; without it the
    // missing-handle rule fires first and this test would pass for the wrong
    // reason.
    assert!(
        policy::check_input(&json!({"action": "type_text", "text": at_limit, "hwnd": 42})).is_ok(),
        "a string exactly at the limit must be accepted"
    );

    let over = "a".repeat(MAX_TYPE_CHARS + 1);
    let err =
        policy::check_input(&json!({"action": "type_text", "text": over, "hwnd": 42})).unwrap_err();
    assert!(err.contains("limit"), "got {err:?}");

    // Multi-byte characters count as one each. A byte count would refuse a
    // shorter, entirely reasonable string.
    let multibyte = "é".repeat(MAX_TYPE_CHARS);
    assert!(
        policy::check_input(&json!({"action": "type_text", "text": multibyte, "hwnd": 42})).is_ok(),
        "the limit is characters, not bytes"
    );
}

#[test]
fn a_key_name_is_read_back_from_the_vocabulary_the_lane_uses() {
    for good in ["enter", "PageUp", "esc", "DEL"] {
        assert!(
            policy::check_input(&json!({"action": "press_key", "key": good, "hwnd": 42})).is_ok(),
            "{good} should be a usable key name"
        );
    }
    let err =
        policy::check_input(&json!({"action": "press_key", "key": "f13", "hwnd": 42})).unwrap_err();
    assert!(err.contains("does not know"), "got {err:?}");
}

#[test]
fn a_click_needs_a_point_and_a_button_this_tool_has() {
    assert!(
        policy::check_input(&json!({"action": "click_window", "x": 1, "hwnd": 42})).is_err(),
        "a click with no y has no point"
    );
    assert!(
        policy::check_input(
            &json!({"action": "click_window", "x": 1, "y": 2, "button": "thumb", "hwnd": 42})
        )
        .is_err(),
        "a button name this lane cannot encode must be rejected, not defaulted"
    );
    for good in ["left", "right", "middle"] {
        assert!(
            policy::check_input(
                &json!({"action": "click_window", "x": 1, "y": 2, "button": good, "hwnd": 42})
            )
            .is_ok(),
            "{good} is a button the lane encodes"
        );
    }
}

#[test]
fn launching_refuses_a_blank_program_and_a_hidden_argument() {
    assert!(
        policy::check_input(&json!({"action": "launch_app", "program": "  "})).is_err(),
        "a blank program name starts nothing"
    );
    let err = policy::check_input(&json!({"action": "launch_app", "program": "notepad\" /x"}))
        .unwrap_err();
    assert!(err.contains("quote"), "got {err:?}");
    assert!(
        policy::check_input(&json!({"action": "launch_app", "program": "notepad.exe"})).is_ok()
    );
}

#[test]
fn a_window_handle_of_zero_is_refused_because_it_means_no_window() {
    let err = policy::check_input(&json!({"action": "close_window", "hwnd": 0})).unwrap_err();
    assert!(err.contains("no window"), "got {err:?}");
}

#[test]
fn acting_on_a_window_without_naming_one_is_refused() {
    // The posted lane would fall back to the foreground window here, which is a
    // different window from the one nobody named. Better to refuse the call.
    for action in ["focus_window", "close_window"] {
        let err = policy::check_input(&json!({"action": action})).unwrap_err();
        assert!(err.contains("hwnd"), "{action}: got {err:?}");
    }
    // Same rule for the actions that also carry a payload, with that payload made
    // valid first so it is the handle that fails and not something earlier.
    for case in [
        json!({"action": "type_text", "text": "hi"}),
        json!({"action": "press_key", "key": "enter"}),
        json!({"action": "click_window", "x": 1, "y": 2}),
        json!({"action": "capture_window"}),
    ] {
        let err = policy::check_input(&case).unwrap_err();
        assert!(err.contains("hwnd"), "got {err:?} for {case}");
    }
}

#[test]
fn the_names_that_need_no_window_say_so_without_complaining_about_one() {
    // capture_window is deliberately absent: it only reads, but it reads one
    // named window, so the handle rule applies to it too.
    for action in ["list_windows", "seat_report", "describe_desktop"] {
        assert!(
            policy::check_input(&json!({"action": action})).is_ok(),
            "{action} takes no window and must not require one"
        );
    }
    assert!(policy::check_input(&json!({"action": "launch_app", "program": "notepad"})).is_ok());
}

#[test]
fn the_summary_names_the_target_and_what_will_happen_to_it() {
    let line = policy::summarize(&json!({"action": "type_text", "hwnd": 1234, "text": "dir /w"}));
    assert!(
        line.contains("1234"),
        "the human needs the window: {line:?}"
    );
    assert!(line.contains("dir /w"), "and the payload: {line:?}");

    let click = policy::summarize(
        &json!({"action": "click_window", "hwnd": 9, "x": 40, "y": 12, "button": "right"}),
    );
    assert!(
        click.contains("right") && click.contains("40") && click.contains("12"),
        "{click:?}"
    );
}

#[test]
fn a_long_payload_is_summarised_instead_of_pasted_whole() {
    let huge = "x".repeat(500);
    let line = policy::summarize(&json!({"action": "type_text", "hwnd": 1, "text": huge}));
    assert!(
        line.chars().count() < 200,
        "an approval prompt has to stay readable, got {} chars",
        line.chars().count()
    );
}

#[test]
fn a_summary_of_input_with_no_action_does_not_panic() {
    // Approval can be asked for a malformed call: the hook runs before parsing.
    // It must answer, not crash.
    let value: Value = json!({});
    let line = policy::summarize(&value);
    assert!(line.contains("missing"), "got {line:?}");
}

#[test]
fn a_seat_with_no_screen_refuses_every_action_that_could_be_misread() {
    // The invariant, not one example per action: a name added to the Windows half
    // later gets refused on a dead seat by default, exactly like the ones here.
    for action in policy::KNOWN_ACTIONS {
        let outcome = policy::check_seat(action, false);
        if *action == "seat_report" {
            assert!(
                outcome.is_ok(),
                "{action} is the question about the seat, so it has to answer even without one"
            );
        } else {
            let reason = outcome.unwrap_err();
            assert!(
                reason.contains("seat_report"),
                "{action} was refused without naming the action that can explain the refusal: {reason}"
            );
        }
    }
}

#[test]
fn a_seat_that_exists_does_not_make_anything_free() {
    // The seat gate answers "is this possible here". It must never quietly
    // become the approval gate too, or a human at a real desktop stops being
    // asked and starts being surprised.
    for action in policy::KNOWN_ACTIONS {
        assert!(
            policy::check_seat(action, true).is_ok(),
            "{action} was refused on a seat that has a desktop"
        );
    }
}

#[test]
fn the_seat_gate_and_the_approval_gate_hold_at_the_same_time() {
    // Two independent facts about one invocation, checked together because the
    // tool asks them in this order and a change to either list can break the
    // combination silently.
    assert!(
        policy::requires_approval("click_window"),
        "moving a human's mouse has to be asked"
    );
    assert!(
        policy::check_seat("click_window", false).is_err(),
        "and it cannot be done where there is no screen"
    );
    assert!(policy::check_seat("click_window", true).is_ok());

    assert!(
        !policy::requires_approval("list_windows"),
        "looking does not interrupt anyone"
    );
    assert!(
        policy::check_seat("list_windows", false).is_err(),
        "but an empty answer from a seat with no desktop is the lie this gate exists to stop"
    );

    assert!(
        policy::check_seat("seat_report", false).is_ok(),
        "asking about the seat is how a refusal like the one above gets explained"
    );
}
