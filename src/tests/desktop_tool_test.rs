//! The `windows_desktop` tool's gates, tested where a test can run.
//!
//! Nothing here touches a desktop. What is asserted is the decision layer the
//! harness consults before it would let anything near one: which invocations ask
//! a human, which arguments get refused before a syscall sees them, and what the
//! tool says when the seat has no screen. That last one is the reason this file
//! exists at all: an agent that cannot see a desktop must be told so, not handed
//! an empty list to act on.

use crate::brain::tools::error::ToolError;
use crate::brain::tools::windows_desktop::WindowsDesktopTool;
use crate::brain::tools::{Tool, ToolCapability, ToolExecutionContext};
use crate::desktop::policy;
use serde_json::Value;

fn tool() -> WindowsDesktopTool {
    WindowsDesktopTool
}

fn ctx() -> ToolExecutionContext {
    ToolExecutionContext::new(uuid::Uuid::new_v4())
}

/// The names that change somebody's machine, and the ones that do not. Kept as
/// two lists so a mistake in `policy::READ_ONLY` shows up as a failing assert
/// rather than as a prompt an agent learns to click through.
const READ_ONLY: [&str; 4] = [
    "list_windows",
    "capture_window",
    "seat_report",
    "describe_desktop",
];
const TOUCHING: [&str; 6] = [
    "click_window",
    "type_text",
    "press_key",
    "launch_app",
    "focus_window",
    "close_window",
];

/// A call that will survive argument validation, so that a refusal further along
/// the chain can be attributed to the gate under test and not to a typo.
fn a_valid_call_for(action: &str) -> Value {
    match action {
        "capture_window" | "focus_window" | "close_window" => {
            serde_json::json!({ "action": action, "hwnd": 131 })
        }
        "click_window" => serde_json::json!({ "action": action, "hwnd": 131, "x": 40, "y": 90 }),
        "type_text" => serde_json::json!({ "action": action, "hwnd": 131, "text": "hello" }),
        "press_key" => serde_json::json!({ "action": action, "hwnd": 131, "key": "enter" }),
        "launch_app" => serde_json::json!({ "action": action, "program": "notepad.exe" }),
        other => serde_json::json!({ "action": other }),
    }
}

#[test]
fn a_read_only_action_does_not_interrupt_a_human() {
    for action in READ_ONLY {
        let input = a_valid_call_for(action);
        assert!(
            !tool().requires_approval_for_input(&input),
            "{action} cannot change anything, so it must not ask"
        );
    }
}

#[test]
fn every_action_that_touches_the_machine_asks_first() {
    for action in TOUCHING {
        let input = a_valid_call_for(action);
        assert!(
            tool().requires_approval_for_input(&input),
            "{action} moves a cursor, types a character, starts a program, or closes a window"
        );
    }
}

#[test]
fn an_action_that_cannot_be_read_is_asked_about() {
    // The direction of the failure is the whole point: an action name nobody
    // registered yet must not be able to slip through as "read-only".
    for input in [
        serde_json::json!({}),
        serde_json::json!({ "action": 7 }),
        serde_json::json!({ "action": "rm_the_desktop" }),
    ] {
        assert!(
            tool().requires_approval_for_input(&input),
            "unreadable input {input} has to be asked about"
        );
    }
}

#[test]
fn the_tool_level_flag_is_not_the_gate() {
    // If this flips to true, every `list_windows` pays a prompt and the approval
    // budget gets spent on reading. The per-invocation hook is the gate.
    assert!(!tool().requires_approval());
    assert_eq!(tool().name(), "windows_desktop");
    assert_eq!(
        tool().capabilities(),
        vec![ToolCapability::SystemModification],
        "it modifies the machine under somebody, not a file"
    );
}

#[test]
fn the_schema_offers_exactly_the_actions_the_policy_knows() {
    // Drift guard: the action list is written out as literal JSON because a
    // schema has to be literal, so this is what keeps the two from disagreeing.
    let listed: Vec<&str> = tool().input_schema()["properties"]["action"]["enum"]
        .as_array()
        .expect("the action enum has to be an array")
        .iter()
        .map(|value| value.as_str().expect("every action name is a string"))
        .collect();
    assert_eq!(listed, policy::KNOWN_ACTIONS.to_vec());
}

#[test]
fn arguments_the_syscall_lane_cannot_honour_are_refused() {
    let refusals = [
        serde_json::json!({ "action": "nope" }),
        serde_json::json!({ "action": "list_windows", "hwnd": 0 }),
        serde_json::json!({ "action": "type_text", "hwnd": 1, "text": "" }),
        serde_json::json!({ "action": "type_text", "hwnd": 1, "text": "two\nlines" }),
        serde_json::json!({ "action": "press_key", "hwnd": 1, "key": "f13" }),
        serde_json::json!({ "action": "click_window", "hwnd": 1, "x": 4 }),
        serde_json::json!({ "action": "focus_window" }),
        serde_json::json!({ "action": "launch_app", "program": "\"cmd.exe\"" }),
    ];
    for input in refusals {
        let verdict = tool().validate_input(&input);
        assert!(
            matches!(verdict, Err(ToolError::InvalidInput(_))),
            "{input} should be refused as invalid, got {verdict:?}"
        );
    }
}

#[test]
fn an_honest_call_passes_validation() {
    for action in READ_ONLY.iter().copied().chain(TOUCHING.iter().copied()) {
        let input = a_valid_call_for(action);
        if let Err(why) = tool().validate_input(&input) {
            panic!("{input} should pass validation, got {why}");
        }
    }
}

/// The refusal this whole surface exists to produce. Windows CI cannot assert it
/// (a runner may have a seat), and the Linux job has no desktop at all, which is
/// exactly the condition under test.
#[cfg(not(windows))]
#[tokio::test]
async fn a_seat_with_no_screen_refuses_everything_but_the_seat_question() {
    assert!(
        !crate::desktop::interactive_session(),
        "this test only means something where there is no desktop"
    );
    let context = ctx();
    for action in READ_ONLY
        .iter()
        .copied()
        .filter(|name| *name != "seat_report")
        .chain(TOUCHING.iter().copied())
    {
        let verdict = tool().execute(a_valid_call_for(action), &context).await;
        let Err(ToolError::PermissionDenied(why)) = verdict else {
            panic!("{action} on a seat with no screen has to refuse, got {verdict:?}");
        };
        assert!(
            why.contains("seat_report"),
            "the refusal has to name the call that can answer: {why}"
        );
    }

    // ...and the one name that works from anywhere must actually work, so this
    // gate is provably not a blanket refusal.
    let report = tool()
        .execute(serde_json::json!({ "action": "seat_report" }), &context)
        .await
        .expect("seat_report is answerable from any process");
    assert!(
        report.output.contains("no interactive desktop"),
        "the report has to say what the seat is: {}",
        report.output
    );
}

/// Argument auditing must not happen at the cost of spawning something: the seat
/// question is settled first, so a headless process cannot start a program on a
/// desktop it cannot see.
#[cfg(not(windows))]
#[tokio::test]
async fn the_seat_is_decided_before_anything_is_spawned() {
    let input = serde_json::json!({
        "action": "launch_app",
        "program": "notepad.exe",
        "args": ["notes.txt"]
    });
    let verdict = tool().execute(input, &ctx()).await;
    assert!(
        matches!(verdict, Err(ToolError::PermissionDenied(_))),
        "a seatless process refuses the seat before reaching the spawn: got {verdict:?}"
    );
}
