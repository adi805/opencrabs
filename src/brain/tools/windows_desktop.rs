//! windows_desktop — see and drive the desktop this process is attached to.
//!
//! Three things this file inherits rather than invents, each of them a rule that
//! already lives somewhere it can be tested:
//!
//! 1. The approval question is answered per invocation by [`policy`], before the
//!    harness runs anything. Read-only names run free; anything that moves a
//!    cursor, types, starts a program, or asks a window to close gets asked
//!    first. An input this file cannot read is also asked about, because failing
//!    toward "interrupt a human" is the only safe direction for an action name
//!    nobody has registered yet.
//! 2. The seat question is answered before any syscall. A process with no window
//!    station does not get an error, it gets an empty window list, and an agent
//!    that reads that as "nothing is open" will act on it. So `seat_report` is
//!    the only name that works everywhere and every other name refuses with the
//!    reason, not with an empty result.
//! 3. A result says what was measured, never what was requested. `SendInput`
//!    taking events or `PostMessageW` queueing them is not proof that a window
//!    did anything, and the focus and close lanes report verdicts the desktop
//!    returned, including the ones that say the action did not land.
//!
//! On a non-Windows build the calls refuse inside `crate::desktop`'s disabled
//! lane, so this file compiles and runs there too. What it never does is claim a
//! desktop it does not have.

use crate::brain::tools::error::{Result, ToolError};
use crate::brain::tools::r#trait::{Tool, ToolCapability, ToolExecutionContext, ToolResult};
use crate::desktop::{self, app, policy};
use async_trait::async_trait;
use serde_json::Value;
use std::path::Path;

/// How many windows one listing prints. The ceiling in [`desktop::MAX_WINDOWS`]
/// is already applied by the enumeration; this is about not dumping 256 lines
/// into a model's context when the caller wanted a short answer.
const LISTED_LINES: usize = 40;

pub struct WindowsDesktopTool;

#[async_trait]
impl Tool for WindowsDesktopTool {
    fn name(&self) -> &str {
        "windows_desktop"
    }

    fn description(&self) -> &str {
        "Look at and drive the desktop this process is sitting on. \
         Actions: list_windows, capture_window, seat_report and describe_desktop \
         are read-only; click_window, type_text, press_key, focus_window, \
         launch_app and close_window change what a person sees and are asked \
         about first. Always call list_windows or seat_report before acting: a \
         process that is not attached to a desktop cannot see any window, and an \
         empty list from there means 'no seat', not 'nothing open'. A returned \
         count is what the OS accepted, never proof an application acted on it; \
         capture_window is how you check."
    }

    fn input_schema(&self) -> Value {
        // The action list is written out rather than borrowed from
        // `policy::KNOWN_ACTIONS` because a schema has to be literal JSON here.
        // The cost is drift, so there is a test that compares the two.
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": [
                        "list_windows",
                        "capture_window",
                        "seat_report",
                        "describe_desktop",
                        "click_window",
                        "type_text",
                        "press_key",
                        "launch_app",
                        "focus_window",
                        "close_window"
                    ]
                },
                "hwnd": { "type": "integer", "description": "Window handle from list_windows; never 0" },
                "x": { "type": "integer", "description": "Screen x for click_window" },
                "y": { "type": "integer", "description": "Screen y for click_window" },
                "button": { "type": "string", "enum": ["left", "right", "middle"] },
                "text": { "type": "string", "description": "Text for type_text, no newlines" },
                "key": {
                    "type": "string",
                    "enum": ["enter", "tab", "escape", "backspace", "delete", "insert",
                             "home", "end", "pageup", "pagedown", "up", "down", "left",
                             "right", "alt"]
                },
                "program": { "type": "string", "description": "Program for launch_app, literal path or name" },
                "args": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Arguments for launch_app, handed literally, never to a shell"
                }
            },
            "required": ["action"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::SystemModification]
    }

    /// The tool as a whole is not an approval question: half of its names cannot
    /// change anything. The gate is per invocation, in
    /// [`Tool::requires_approval_for_input`], and that is the hook the tool loop
    /// and the parallel path actually call.
    fn requires_approval(&self) -> bool {
        false
    }

    fn requires_approval_for_input(&self, input: &Value) -> bool {
        match policy::action_of(input) {
            Ok(action) => policy::requires_approval(&action),
            // An action we could not read is an action we cannot vouch for.
            Err(_) => true,
        }
    }

    fn validate_input(&self, input: &Value) -> Result<()> {
        policy::check_input(input).map_err(ToolError::InvalidInput)
    }

    async fn execute(&self, input: Value, context: &ToolExecutionContext) -> Result<ToolResult> {
        let action = policy::action_of(&input).map_err(ToolError::InvalidInput)?;
        // Checked again rather than trusting the caller: `validate_input` is a
        // hook the harness may or may not have run, and a syscall lane that acts
        // on an argument it never saw is worse than a refusal.
        policy::check_input(&input).map_err(ToolError::InvalidInput)?;
        let interactive = desktop::interactive_session();
        policy::check_seat(&action, interactive).map_err(ToolError::PermissionDenied)?;

        let report = match action.as_str() {
            "seat_report" => seat_report(interactive),
            "list_windows" => list_report().map_err(ToolError::Execution)?,
            "describe_desktop" => describe_report(interactive).map_err(ToolError::Execution)?,
            "capture_window" => capture_report(hwnd_of(&input), &context.working_dir())
                .map_err(ToolError::Execution)?,
            "click_window" => {
                let at = desktop::ScreenPoint {
                    x: input.get("x").and_then(Value::as_i64).unwrap_or(0) as i32,
                    y: input.get("y").and_then(Value::as_i64).unwrap_or(0) as i32,
                };
                let delivery = desktop::click_window(hwnd_of(&input), button_of(&input), at)
                    .map_err(|why| ToolError::Execution(why.to_string()))?;
                delivery_note(delivery)
            }
            "type_text" => {
                let text = input
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let delivery = desktop::type_into_window(hwnd_of(&input), &text)
                    .map_err(|why| ToolError::Execution(why.to_string()))?;
                delivery_note(delivery)
            }
            "press_key" => {
                let key = input.get("key").and_then(Value::as_str).unwrap_or_default();
                let key = desktop::Key::parse(key).ok_or_else(|| {
                    ToolError::InvalidInput(format!("press_key does not know {key:?}"))
                })?;
                let delivery = desktop::press_key_in_window(hwnd_of(&input), key)
                    .map_err(|why| ToolError::Execution(why.to_string()))?;
                delivery_note(delivery)
            }
            "focus_window" => {
                let verdict =
                    desktop::focus_target(hwnd_of(&input)).map_err(ToolError::Execution)?;
                format!(
                    "focus verdict from the desktop, not from our call: {verdict:?}. \
                     Anything other than Confirmed means it did not land; the OS \
                     foreground lock is a rule, not a bug to retry."
                )
            }
            "launch_app" => {
                let program = input
                    .get("program")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let args = args_of(&input).map_err(ToolError::InvalidInput)?;
                let plan =
                    app::LaunchPlan::build(&program, &args).map_err(ToolError::InvalidInput)?;
                let (pid, window) = desktop::launch_to_window(&plan, app::LAUNCH_WINDOW_SETTLE)
                    .map_err(ToolError::Execution)?;
                let target = match window {
                    Some(w) => format!(
                        "made window hwnd={} pid={} [{}] {:?}",
                        w.hwnd, w.pid, w.class, w.title
                    ),
                    None => "started and made no window inside the settle budget, which means \
                            headless or still starting, not a failure"
                        .to_string(),
                };
                format!(
                    "launched {} as pid {pid}; the window {}",
                    plan.describe(),
                    target
                )
            }
            "close_window" => close_report(hwnd_of(&input)).map_err(ToolError::Execution)?,
            // Reachable only if KNOWN_ACTIONS gains a name before this match does,
            // which is a build-shape mistake, so it is a loud error not a refusal.
            other => {
                return Err(ToolError::Internal(format!(
                    "windows_desktop knows the action {other:?} but has no executor for it"
                )));
            }
        };
        Ok(ToolResult::success(report))
    }
}

/// The handle the input names. `policy::check_input` already required it to be
/// present and non-zero for every action that reaches here.
fn hwnd_of(input: &Value) -> isize {
    input.get("hwnd").and_then(Value::as_i64).unwrap_or(0) as isize
}

fn button_of(input: &Value) -> desktop::MouseButton {
    match input.get("button").and_then(Value::as_str) {
        Some("right") => desktop::MouseButton::Right,
        Some("middle") => desktop::MouseButton::Middle,
        // `check_input` accepts a missing button and the lanes treat it as left.
        _ => desktop::MouseButton::Left,
    }
}

/// The argument list, read literally. Anything that is not a string is a
/// refusal: silently stringifying a number or a nested object would send the
/// program a different command line than the one the caller wrote.
fn args_of(input: &Value) -> std::result::Result<Vec<String>, String> {
    match input.get("args") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| format!("launch_app args must be strings, got {item}"))
            })
            .collect(),
        Some(other) => Err(format!("launch_app args must be an array, got {other}")),
    }
}

/// What the seat actually is. The only answer that can be given from anywhere.
fn seat_report(interactive: bool) -> String {
    if interactive {
        "this process is attached to an interactive desktop: window enumeration \
         and capture can be trusted."
            .to_string()
    } else {
        "this process has no interactive desktop to look at, so an empty window \
         list here means the seat has no screen, not that nothing is open. \
         Every action except seat_report is refused for that reason, and that \
         refusal is the finding, not a failure to retry."
            .to_string()
    }
}

fn list_report() -> std::result::Result<String, String> {
    let list = desktop::list_windows().map_err(|why| why.to_string())?;
    let mut out = format!(
        "{} window(s) on this seat (ceiling {}){}: ",
        list.len(),
        desktop::MAX_WINDOWS,
        if list.truncated {
            ", TRUNCATED: more exist than are listed"
        } else {
            ""
        },
    );
    for window in list.windows.iter().take(LISTED_LINES) {
        out.push_str(&format!(
            "\n  hwnd={} pid={} {} [{}] {}",
            window.hwnd,
            window.pid,
            if window.foreground {
                "FOREGROUND"
            } else {
                "          "
            },
            window.class,
            window.title,
        ));
    }
    if list.windows.len() > LISTED_LINES {
        out.push_str(&format!(
            "\n  ({} more not printed)",
            list.windows.len() - LISTED_LINES
        ));
    }
    Ok(out)
}

fn describe_report(interactive: bool) -> std::result::Result<String, String> {
    let seat = seat_report(interactive);
    let list = desktop::list_windows().map_err(|why| why.to_string())?;
    let console_hosts = list
        .windows
        .iter()
        .filter(|w| app::is_console_host(&w.class))
        .count();
    // Only what this snapshot actually measured. The enumeration filters the
    // shell backdrop and off-screen windows before handing the list over, and
    // it does not report how many it dropped, so nothing here says so.
    Ok(format!(
        "{seat} Listed {} window(s) of a ceiling of {}, {} console host(s) among them ({}), {}.",
        list.len(),
        desktop::MAX_WINDOWS,
        console_hosts,
        app::CONSOLE_HOST_CLASSES.join(" or "),
        if list.truncated {
            "the ceiling was reached, so more exist"
        } else {
            "the list is whole"
        },
    ))
}

fn capture_report(hwnd: isize, out_dir: &Path) -> std::result::Result<String, String> {
    let path = out_dir.join(format!("desktop-capture-{hwnd}.png"));
    let capture = desktop::capture_window_to_png(hwnd, &path).map_err(|why| why.to_string())?;
    Ok(format!(
        "photographed window {hwnd} at {}x{}, ink ratio {:.4}, {}; written to {}",
        capture.width,
        capture.height,
        capture.ink_ratio,
        if capture.is_blank() {
            "blank: nothing was drawn, so this window is not a place an action landed"
        } else {
            "has ink: something is being drawn"
        },
        path.display(),
    ))
}

fn close_report(hwnd: isize) -> std::result::Result<String, String> {
    let list = desktop::list_windows().map_err(|why| why.to_string())?;
    let Some(target) = list.windows.iter().find(|w| w.hwnd == hwnd) else {
        // The listing is filtered, so absence is not proof of a closed window.
        // Saying so is the point: posting to a handle we cannot describe is a
        // guess about somebody else's window.
        return Err(format!(
            "window {hwnd} is not in the enumerated list, which means it is filtered out or \
             already gone, not that we can name it; nothing was posted"
        ));
    };
    let verdict = desktop::close_target(target, std::process::id(), app::CLOSE_SETTLE)?;
    Ok(format!(
        "close verdict from the desktop, not from our call: {verdict:?}. StillHere means the \
         application is deciding (a save, a prompt), not that the request failed; NotDelivered \
         means the queue refused it and nothing was asked."
    ))
}

/// How many the OS took, with the limit of that claim attached.
fn delivery_note(delivery: desktop::Delivery) -> String {
    match delivery {
        desktop::Delivery::Injected { events } => format!(
            "injected {events} events into the input stream. The OS accepted them; that is not \
             proof a window acted on them, so capture_window is how you check."
        ),
        desktop::Delivery::Queued { messages } => format!(
            "queued {messages} messages to the window. A queue accepts messages even from a hung \
             window and discards ones it has no handler for, so this says nothing about effect; \
             capture_window is how you check."
        ),
    }
}
