//! The decisions behind the app-control actions: what we are willing to start,
//! and what it actually takes to say one of them worked.
//!
//! This file holds no syscalls, on purpose. Starting a program, moving the
//! foreground, and asking a window to close are each a two-part operation: a
//! call that *requests* something, and a reading that *confirms* it. The request
//! half can only run on a machine with a desktop, which makes it expensive to
//! test and unforgiving to get wrong. The confirmation half is arithmetic over
//! values already read from the OS, so it lives here where the ordinary Linux
//! test job can run every branch of it.
//!
//! That split is the whole reason this module exists. Every rule below came from
//! a case where a naive reading of a Windows return value would have told a
//! caller something false:
//!
//! * `SetForegroundWindow` returns a BOOL that reports whether the request was
//!   *accepted*, not whether focus moved. Windows blocks focus changes from a
//!   background process (the foreground lock) and the call can succeed while the
//!   caret stays exactly where it was.
//! * `PostMessageW(WM_CLOSE)` returning true means the message was queued. An
//!   application that is showing a "save changes?" dialog takes the message and
//!   does not close, which is a correct application and a misleading receipt.
//! * A window can be "found" by title alone and still be a `#32770` error
//!   dialog: that class was measured on the CI runner, where a mistyped `start`
//!   argument produced a dialog whose caption contained the very string the test
//!   was polling for. Title matching without a class check can be satisfied by
//!   the operating system complaining about us.
//!
//! The launch path never builds a shell command line, so shell
//! metacharacters (`&`, `|`, `>`) are inert here and are *not* rejected: they are
//! legal characters in a Windows file name, and refusing them would break the
//! one use case that matters, a path with a space or a bracket in it.

use std::fmt;
use std::time::Duration;

use super::model::WindowInfo;

/// Window classes that host a console.
///
/// Both are real and both have been seen in this project's own measurements:
/// `ConsoleWindowClass` is conhost, and a runner with Windows Terminal installed
/// hands every new console to `CASCADIA_HOSTING_WINDOW_CLASS` instead, which is
/// also why a console opened by one test can appear as a tab in a window another
/// test is watching.
pub const CONSOLE_HOST_CLASSES: &[&str] = &["ConsoleWindowClass", "CASCADIA_HOSTING_WINDOW_CLASS"];

/// Whether a class name belongs to a window that can host a console.
///
/// This is the check that turns "a window whose title contains our marker
/// appeared" from a claim into a receipt. Anything else, including a dialog
/// raised by the launcher itself, is not the console we asked for.
pub fn is_console_host(class: &str) -> bool {
    CONSOLE_HOST_CLASSES
        .iter()
        .any(|known| known.eq_ignore_ascii_case(class))
}

/// How many arguments one launch request may carry.
///
/// A ceiling rather than a filter: an agent that has assembled hundreds of
/// arguments has misunderstood the task, and the honest thing to do is say so
/// instead of launching something neither of us was thinking about.
pub const MAX_LAUNCH_ARGS: usize = 32;

/// The documented ceiling for a `CreateProcessW` command line, in characters.
///
/// Beyond it Windows truncates rather than refusing, which is the worst possible
/// outcome for a launch: the program starts with half an argument list and every
/// later symptom points somewhere else.
pub const MAX_COMMAND_LINE_CHARS: usize = 32_767;

/// A validated program plus the arguments to hand it.
///
/// Built only through [`LaunchPlan::build`], so no caller can hold one that has
/// not passed the checks, and the syscall lane's job reduces to
/// `Command::new(plan.program).args(&plan.args)`. Nothing here is ever joined
/// into a string and handed to `cmd.exe`: that round trip is what mangles quotes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchPlan {
    pub program: String,
    pub args: Vec<String>,
}

/// Reject a value that cannot survive being passed through as one argv token.
///
/// A null byte ends the string at the FFI boundary, silently. A newline is the
/// same hazard on the paths that go through a console. A quote is different in
/// kind: Windows forbids it in file names outright, so one appearing here means
/// the caller built a command line rather than a path, and letting it through
/// would move an argument boundary.
fn reject_unrepresentable(what: &str, value: &str) -> Result<(), String> {
    for (bad, why) in [
        (
            '\0',
            "contains a null byte, which the OS reads as the end of the string",
        ),
        ('\n', "contains a newline"),
        ('\r', "contains a carriage return"),
        (
            '"',
            "contains a quote; pass the path itself, not a quoted command line",
        ),
    ] {
        if value.contains(bad) {
            return Err(format!("launch {what} {why}"));
        }
    }
    Ok(())
}

impl LaunchPlan {
    /// Validate a program and its arguments for one launch.
    ///
    /// An empty *argument* is allowed and kept: `argv` can genuinely carry an
    /// empty string, and Rust's `Command` quotes it so it survives. An argument
    /// ending in a backslash is also allowed, because the standard library
    /// doubles the backslashes that would otherwise escape the closing quote.
    pub fn build(program: &str, args: &[String]) -> Result<Self, String> {
        let trimmed = program.trim();
        if trimmed.is_empty() {
            return Err("launch needs a program name; an empty one starts nothing".into());
        }
        reject_unrepresentable("program", trimmed)?;
        if args.len() > MAX_LAUNCH_ARGS {
            return Err(format!(
                "launch got {} arguments, the limit is {MAX_LAUNCH_ARGS}",
                args.len()
            ));
        }
        let mut total = trimmed.chars().count();
        for (index, arg) in args.iter().enumerate() {
            reject_unrepresentable(&format!("argument {index}"), arg)?;
            total += arg.chars().count();
        }
        if total > MAX_COMMAND_LINE_CHARS {
            return Err(format!(
                "launch would build a {total}-character command line, and Windows allows {MAX_COMMAND_LINE_CHARS}"
            ));
        }
        Ok(Self {
            program: trimmed.to_string(),
            args: args.to_vec(),
        })
    }

    /// The line a human reads before approving a launch.
    ///
    /// Truncated on purpose, for the same reason `policy::summarize` truncates:
    /// an approval prompt that buries the decision in a 30 KB argument list gets
    /// clicked through without being read.
    pub fn describe(&self) -> String {
        const SHOW: usize = 6;
        let head: Vec<String> = self
            .args
            .iter()
            .take(SHOW)
            .map(|arg| format!("{arg:?}"))
            .collect();
        let more = if self.args.len() > SHOW {
            format!(", and {} more", self.args.len() - SHOW)
        } else {
            String::new()
        };
        if head.is_empty() {
            format!("starting {:?}", self.program)
        } else {
            format!(
                "starting {:?} with {} argument(s): {}{more}",
                self.program,
                self.args.len(),
                head.join(" ")
            )
        }
    }
}

/// What happened when we tried to give one window the foreground.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusVerdict {
    /// The OS now says this window holds keyboard focus. The only one that means
    /// the action did what it claimed.
    Confirmed { hwnd: isize },
    /// The call was accepted and something else is still in front. This is the
    /// foreground lock, not a bug in us: report it, do not retry silently.
    HeldByAnother { wanted: isize, actual: isize },
    /// Nothing had focus when we looked.
    NothingHoldsFocus { wanted: isize },
}

/// Turn the two numbers into the claim.
///
/// `actual` is `GetForegroundWindow`, where 0 means "no window" and is mapped to
/// [`FocusVerdict::NothingHoldsFocus`] rather than being compared as a handle.
pub fn focus_verdict(wanted: isize, actual: isize) -> FocusVerdict {
    if actual == 0 {
        FocusVerdict::NothingHoldsFocus { wanted }
    } else if actual == wanted {
        FocusVerdict::Confirmed { hwnd: wanted }
    } else {
        FocusVerdict::HeldByAnother { wanted, actual }
    }
}

impl fmt::Display for FocusVerdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Confirmed { hwnd } => write!(f, "window {hwnd} is now the foreground window"),
            Self::HeldByAnother { wanted, actual } => write!(
                f,
                "focus did not move: window {wanted} was asked for and {actual} still holds it \
                 (Windows blocks foreground changes from a background process)"
            ),
            Self::NothingHoldsFocus { wanted } => {
                write!(f, "no window holds focus after asking for {wanted}")
            }
        }
    }
}

/// What happened when we asked one window to close.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseVerdict {
    /// It is no longer in the window list.
    Gone { hwnd: isize },
    /// The request was delivered and the window is still there. Not a failure to
    /// hide: the application may be mid-save, or asking a human a question.
    StillHere { hwnd: isize },
    /// The queue refused the message, so the application never saw it.
    NotDelivered { hwnd: isize },
}

/// Build the verdict from what the two syscalls reported.
///
/// A `posted` of false wins over the listing: a window that is absent *and*
/// undeliverable tells us the handle was already stale, and the useful message
/// is "we never asked anything", not a claim that our request worked.
pub fn close_verdict(hwnd: isize, posted: bool, still_listed: bool) -> CloseVerdict {
    if !posted {
        CloseVerdict::NotDelivered { hwnd }
    } else if still_listed {
        CloseVerdict::StillHere { hwnd }
    } else {
        CloseVerdict::Gone { hwnd }
    }
}

impl fmt::Display for CloseVerdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gone { hwnd } => write!(f, "window {hwnd} closed"),
            Self::StillHere { hwnd } => write!(
                f,
                "window {hwnd} was asked to close and is still open \
                 (it may be waiting on a human: a save prompt, or a modal dialog)"
            ),
            Self::NotDelivered { hwnd } => {
                write!(f, "the close request for window {hwnd} never reached it")
            }
        }
    }
}

/// May we ask this window to close?
///
/// Three refusals, each for a way this action could damage something the caller
/// did not mean to touch:
///
/// * Handle `0` is what the API uses for "there is no window", so it is never a
///   target. Policy rejects it before approval is even asked; this is the second
///   gate, on the resolved window rather than the raw argument.
/// * A window whose owner we cannot read (`pid == 0`, the value the backend uses
///   for a failed query) cannot be checked against the two rules below, and a
///   close is not reversible.
/// * Our own process. On a runner this is the difference between closing a window
///   and ending the job: the standard handles the test binary talks through
///   belong to a console, and the process that owns them is the one enumerated.
///
/// Desktop furniture is refused too, by the same rule [`super::is_shell_backdrop`]
/// already uses for what is worth listing: `Progman` and `Shell_TrayWnd` are not
/// documents, and closing them takes the session's shell with them.
pub fn close_target_allowed(target: &WindowInfo, our_pid: u32) -> Result<(), String> {
    if target.hwnd == 0 {
        return Err("hwnd 0 is the value for \"no window\", not a window".into());
    }
    if target.pid == 0 {
        return Err(format!(
            "window {} has no readable owner, so nothing can be promised about what a close would reach",
            target.hwnd
        ));
    }
    if target.pid == our_pid {
        return Err(format!(
            "window {} belongs to this process (pid {our_pid}); closing it can take the session we are running in",
            target.hwnd
        ));
    }
    if super::is_shell_backdrop(&target.class) {
        return Err(format!(
            "window {} is desktop furniture ({}) and has no business being closed",
            target.hwnd, target.class
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Waiting, and which window belongs to the child we started
// ---------------------------------------------------------------------------

/// How long the syscall lane waits before looking at the desktop again.
///
/// One look is never enough. A process exists before any window it will create
/// exists, and a window that has been asked to close stays up until its owner
/// reads its queue, so a reading taken at the wrong moment reports the opposite
/// of the truth. This interval is what turns "not yet" into a measured answer
/// instead of a guess.
pub const WINDOW_POLL: Duration = Duration::from_millis(100);

/// How long a launched program has to produce a window.
///
/// Sized to a program starting rather than to this crate's own build. The
/// consoles measured on the runner appear in under a second, and a program with
/// a splash screen takes longer than that on purpose. Past this the honest
/// answer is "it started and showed nothing", not a longer wait.
pub const LAUNCH_WINDOW_SETTLE: Duration = Duration::from_secs(25);

/// How long to keep re-reading the foreground window after asking for it.
///
/// The request and the move are not one instant, so a single read taken
/// immediately could catch the change before the desktop made it and report a
/// refusal that was only a delay.
pub const FOCUS_SETTLE: Duration = Duration::from_secs(2);

/// How long to wait for a window to stop existing after it was asked to close.
///
/// Short on purpose. Past it the answer is "still here", which is true whether
/// the application is showing a save prompt, is wedged, or is ignoring the
/// message, and all three belong to whoever reads the receipt rather than to a
/// longer wait.
pub const CLOSE_SETTLE: Duration = Duration::from_secs(10);

/// The window belonging to a process we started.
///
/// Matching by owner rather than by title is the point. A title is whatever the
/// application decided to call itself: it may not be set yet, it may carry a
/// document name, and it is localised, while the pid is exact from the moment
/// the process exists.
///
/// `pid == 0` is refused, for the same reason [`close_target_allowed`] refuses
/// it. That is the value the backend reports when the owner query failed, so
/// treating it as an owner would match every window whose query failed and
/// return the first of them.
///
/// The largest eligible window wins, because a program that creates several
/// usually creates the invisible ones first: a message-only window, a tray host,
/// or an IME bridge. The one with real area and a real span is the one a person
/// would point at.
pub fn window_of_pid(windows: &[WindowInfo], pid: u32) -> Option<&WindowInfo> {
    if pid == 0 {
        return None;
    }
    windows
        .iter()
        .filter(|window| window.pid == pid && window.hwnd != 0 && window.rect.has_positive_span())
        .max_by_key(|window| window.rect.area())
}
