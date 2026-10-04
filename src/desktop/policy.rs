//! Which desktop actions need a human's approval before they run.
//!
//! The rule this module encodes is the difference between looking and touching.
//! Listing windows, photographing one, and reporting where this process is
//! sitting are all read-only: they cannot change what a person sees on their own
//! screen, so they run without a prompt. Anything that moves a cursor, types a
//! character, starts a program, or asks a window to close changes the machine
//! under someone, and gets asked first.
//!
//! This lives apart from the syscalls for one reason. The gate is the part that
//! must not be wrong, and a gate welded to `SendInput` can only be tested on a
//! machine with a desktop, which is exactly where a mistake is expensive. Kept
//! pure, every branch here runs in the ordinary Linux test job.
//!
//! The action names are strings rather than a parsed enum because the approval
//! hook is asked *before* the tool parses its input: the harness wants to know
//! whether to interrupt a human, and it must be able to answer that even for an
//! argument list that will later be rejected as malformed.

use serde_json::Value;

/// The read-only half of the surface. Everything absent from this list is
/// treated as an action that touches the machine.
const READ_ONLY: &[&str] = &[
    "list_windows",
    "capture_window",
    "seat_report",
    "describe_desktop",
];

/// The actions this tool understands at all.
///
/// Checked separately from approval on purpose: an unknown name should fail as
/// "not something this tool does", not as "blocked pending approval", because
/// the second message invites the human to approve a typo.
pub const KNOWN_ACTIONS: &[&str] = &[
    "list_windows",
    "capture_window",
    "seat_report",
    "describe_desktop",
    "click_window",
    "type_text",
    "press_key",
    "launch_app",
    "focus_window",
    "close_window",
];

/// One character beyond the read-only line: does this invocation need asking?
///
/// Unknown names return `true`. Failing toward "ask the human" is the only safe
/// direction for a name that is not in the list yet, since a future action added
/// to the Windows half and forgotten here would otherwise run unapproved.
pub fn requires_approval(action: &str) -> bool {
    !READ_ONLY.contains(&action)
}

/// The one action that can still be answered when there is no screen to look at.
const SEAT_FREE: &[&str] = &["seat_report"];

/// Whether this action may be attempted from a session that has no desktop.
///
/// `interactive` is the platform lane's reading: our own session id, the console
/// session id, and the window station, in that order. A `false` there is not a
/// failure to retry, it is the reason an empty window list cannot be trusted.
///
/// This refusal exists to stop the worst misreading the surface allows. An agent
/// that called `list_windows` from a Session 0 service would get nothing back,
/// conclude that nothing is open, and act on that. So the read-only actions are
/// refused here for the same reason as the touching ones: not because they are
/// dangerous, but because on a seat with no screen they cannot be answered
/// honestly at all.
///
/// The other half of the seat posture is deliberately not in this function: a
/// human sitting at a real desktop who has not agreed to have their mouse moved
/// is what [`requires_approval`] is for, and every name outside the read-only
/// list already pays it. A seat that exists does not make an action free.
pub fn check_seat(action: &str, interactive: bool) -> Result<(), String> {
    if interactive || SEAT_FREE.contains(&action) {
        return Ok(());
    }
    Err(format!(
        "{action} needs a desktop a person could see, and this process is not attached to one; \
         call seat_report for the session reading"
    ))
}

/// The actions that name one particular window, and so are meaningless without
/// a handle to name it with.
///
/// Checked here rather than in each syscall because the syscall lanes differ: a
/// missing handle on the posted route would quietly target the foreground
/// window instead, which is a different window from the one that was asked for.
const WINDOW_TARGETS: &[&str] = &[
    "capture_window",
    "click_window",
    "type_text",
    "press_key",
    "focus_window",
    "close_window",
];

/// The action this input is asking for, or why it cannot be told.
///
/// Returns the bare name so a caller can gate on it without parsing the rest of
/// the arguments.
pub fn action_of(input: &Value) -> Result<String, String> {
    let action = input
        .get("action")
        .and_then(Value::as_str)
        .ok_or_else(|| "input needs a string field \"action\"".to_string())?;
    if !KNOWN_ACTIONS.contains(&action) {
        return Err(format!(
            "unknown action {action:?}; this tool does: {}",
            KNOWN_ACTIONS.join(", ")
        ));
    }
    Ok(action.to_string())
}

/// The line a human reads before deciding.
///
/// Approval fatigue is real, and the cure is context rather than more warnings:
/// an approval prompt that says only "desktop tool wants to run" teaches a
/// person to click through it. This names the target and the payload, with the
/// text quoted and truncated so a 4 KB paste does not bury the decision.
pub fn summarize(input: &Value) -> String {
    let action = input
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("(missing)");
    let target = match input.get("hwnd").and_then(Value::as_i64) {
        Some(hwnd) => format!("window {hwnd}"),
        None => "the desktop".to_string(),
    };
    let detail = match action {
        "type_text" => {
            let text = input.get("text").and_then(Value::as_str).unwrap_or("");
            let shown: String = text.chars().take(60).collect();
            let ellipsis = if text.chars().count() > 60 { "…" } else { "" };
            format!("typing {shown:?}{ellipsis}")
        }
        "press_key" => format!(
            "pressing {:?}",
            input
                .get("key")
                .and_then(Value::as_str)
                .unwrap_or("(missing)")
        ),
        "click_window" => format!(
            "clicking {:?} at {},{}",
            input
                .get("button")
                .and_then(Value::as_str)
                .unwrap_or("left"),
            input.get("x").and_then(Value::as_i64).unwrap_or(0),
            input.get("y").and_then(Value::as_i64).unwrap_or(0),
        ),
        "launch_app" => format!(
            "starting {:?}",
            input
                .get("program")
                .and_then(Value::as_str)
                .unwrap_or("(missing)")
        ),
        "close_window" => "asking it to close".to_string(),
        "focus_window" => "bringing it forward".to_string(),
        other => other.to_string(),
    };
    format!("{action}: {target}, {detail}")
}

/// The longest text this tool will type in one call.
pub const MAX_TYPE_CHARS: usize = 4096;

/// Check the arguments an action will go on to use.
///
/// Every rejection here is a case where the syscall lane would either do
/// something different from what was asked or hand back a result that cannot be
/// read: a window handle of 0 is what `GetForegroundWindow` returns for "no
/// window", not a window; empty text produces a delivery of zero events that
/// looks like success.
pub fn check_input(input: &Value) -> Result<(), String> {
    let action = action_of(input)?;
    match action.as_str() {
        "type_text" => {
            let text = input
                .get("text")
                .and_then(Value::as_str)
                .ok_or_else(|| "type_text needs a string field \"text\"".to_string())?;
            let count = text.chars().count();
            if count == 0 {
                return Err("type_text refuses empty text: zero events is not a delivery".into());
            }
            if count > MAX_TYPE_CHARS {
                return Err(format!(
                    "type_text got {count} characters, the limit is {MAX_TYPE_CHARS}"
                ));
            }
            // A newline inside a typed string submits whatever came before it.
            // That is a decision about the human's machine, so it has to be the
            // explicit `press_key` route instead of something smuggled in a field.
            if text.contains('\n') || text.contains('\r') {
                return Err(
                    "type_text refuses newlines; press Enter with press_key if it is wanted".into(),
                );
            }
        }
        "press_key" => {
            let key = input
                .get("key")
                .and_then(Value::as_str)
                .ok_or_else(|| "press_key needs a string field \"key\"".to_string())?;
            if crate::desktop::Key::parse(key).is_none() {
                return Err(format!("press_key does not know {key:?}"));
            }
        }
        "click_window" => {
            for field in ["x", "y"] {
                if input.get(field).and_then(Value::as_i64).is_none() {
                    return Err(format!("click_window needs an integer field {field:?}"));
                }
            }
            if let Some(button) = input.get("button").and_then(Value::as_str)
                && !matches!(button, "left" | "right" | "middle")
            {
                return Err(format!("click_window does not know button {button:?}"));
            }
        }
        "launch_app" => {
            let program = input
                .get("program")
                .and_then(Value::as_str)
                .ok_or_else(|| "launch_app needs a string field \"program\"".to_string())?;
            if program.trim().is_empty() {
                return Err("launch_app refuses an empty program name".into());
            }
            // Never handed to a shell, so metacharacters are inert. A leading
            // quote is the one shape that reads like an attempt to hide an
            // argument from a literal path, and it is rejected on those grounds.
            if program.contains('"') {
                return Err("launch_app refuses a program name containing a quote".into());
            }
        }
        // Read-only names, plus the two that only point at a window and carry no
        // payload of their own. Listing them is what keeps the `other` arm below
        // honest: an action added to KNOWN_ACTIONS without a decision here fails
        // as "no argument rules for ..." instead of silently passing validation.
        "list_windows" | "capture_window" | "seat_report" | "describe_desktop" | "focus_window"
        | "close_window" => {}
        other => return Err(format!("no argument rules for {other:?}")),
    }
    // A handle of 0 is the value the API itself uses for "there is no window",
    // so it is never a target. Letting it through would let a call report
    // success against nothing.
    if let Some(hwnd) = input.get("hwnd")
        && hwnd.as_i64().unwrap_or(0) == 0
    {
        return Err("hwnd 0 is the value for \"no window\", not a window".into());
    }
    if WINDOW_TARGETS.contains(&action.as_str()) && input.get("hwnd").is_none() {
        return Err(format!(
            "{action} needs the \"hwnd\" of the window to act on; call list_windows first"
        ));
    }
    Ok(())
}
