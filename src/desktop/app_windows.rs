//! Composing the app-control calls, on Windows.
//!
//! Every decision these functions make is already in [`super::app`]. What is
//! here is the syscalls and the waiting between them: start a program, ask for
//! the foreground, ask a window to close, and ask whether a handle still names a
//! window.
//!
//! Each public function is a request followed by a read-back, and the read-back
//! is the part that matters. A request that returned success only means the OS
//! accepted it, which is a different claim from the thing having happened, so
//! each one loops on a plain reading of the desktop until either the answer
//! arrives or the budget in [`super::app`] runs out. Running out is a reportable
//! outcome, never an error: "asked, and it is still there" is what the caller
//! needs to be told.
//!
//! The launch goes through `std::process::Command` rather than `CreateProcessW`
//! for the same reason the input lane builds its own events: the standard
//! library already solves argv quoting, and a second implementation of that is a
//! second place for a path with a space to break.

use std::time::{Duration, Instant};

use super::app::{self, CloseVerdict, FocusVerdict, LaunchPlan, window_of_pid};
use super::model::WindowInfo;
use super::win32::{GetForegroundWindow, IsWindow, SetForegroundWindow};
use super::{Key, inject_key};

/// Start a program. Returns its process id.
///
/// Never goes through a shell, so a path with `&` or `|` in it is a path and not
/// a pipeline. The pid comes back from the spawn itself, which is exact, instead
/// of being searched for in the window list afterwards.
///
/// All three standard handles are set to null. That is not tidiness: a console
/// program whose stdin is this process's pipe at end of file reads EOF at its
/// first prompt and quits, which is the failure that cost this series two
/// runner cycles. A child that needs a console gets its own.
pub fn launch_app(program: &str, args: &[String]) -> Result<u32, String> {
    let child = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|why| format!("could not start {program:?}: {why}"))?;
    Ok(child.id())
}

/// Ask for a window to be brought to the foreground.
///
/// Private, and dropped on the floor by its only caller. The return value
/// reports whether the *request* was accepted, which is a statement about this
/// process's standing with the desktop rather than about the window, and Windows
/// refuses focus changes from a process that is not already in front. See
/// [`focus_target`] for the value that can actually be read as evidence.
fn set_foreground(hwnd: isize) -> bool {
    unsafe { SetForegroundWindow(hwnd) != 0 }
}

/// The window holding keyboard focus right now, or `0` for none.
fn foreground() -> isize {
    unsafe { GetForegroundWindow() }
}

/// Whether a handle still names a window.
///
/// Asked instead of re-running the enumeration, because that list is filtered: a
/// window can leave it without ceasing to exist, and a filtered-out window is
/// not a closed one.
fn is_window(hwnd: isize) -> bool {
    unsafe { IsWindow(hwnd) != 0 }
}

/// Start a program, then look for the window it made.
///
/// Takes a [`LaunchPlan`] rather than a program name so that no caller can reach
/// the spawn with arguments nobody has checked: a plan can only be built by
/// [`LaunchPlan::build`].
///
/// The window is an `Option`, not an error. A process that made no window is a
/// program that started and happens to be headless, and calling that a failure
/// would contradict what the spawn just reported.
pub fn launch_to_window(
    plan: &LaunchPlan,
    settle: Duration,
) -> Result<(u32, Option<WindowInfo>), String> {
    let pid = launch_app(&plan.program, &plan.args)?;
    let deadline = Instant::now() + settle;
    loop {
        let list = super::list_windows().map_err(|why| format!("could not list windows: {why}"))?;
        if let Some(window) = window_of_pid(&list.windows, pid) {
            return Ok((pid, Some(window.clone())));
        }
        if Instant::now() >= deadline {
            return Ok((pid, None));
        }
        std::thread::sleep(app::WINDOW_POLL);
    }
}

/// Ask for the foreground, then keep reading until the desktop answers.
///
/// `Result` rather than a bare verdict so the platforms with no window manager
/// can refuse the question instead of inventing an answer: `FocusVerdict` has no
/// value that means "no desktop to ask".
pub fn focus_target(hwnd: isize) -> Result<FocusVerdict, String> {
    // Windows does not let a background process move the foreground: the call is
    // accepted and then ignored, which is exactly the failure the read-back below
    // is built to catch. Run 36504919419 measured it on the real desktop, window
    // 852368 asked for while 328230 still held the place. One of the documented
    // exceptions is that the process which received the last input event may set
    // the foreground, so a lone Alt press-and-release goes in first, through the
    // same SendInput path the keyboard receipt is already measured on.
    //
    // The injection's own result is dropped on purpose. Whether Alt arrived is not
    // the claim this function makes; whether the window came forward is, and that
    // is read back from the desktop instead of from either call. A keyboard layout
    // with no scan code for `VK_MENU` therefore shows up as an unconfirmed focus,
    // not as a panic or a false success.
    let _ = inject_key(Key::Alt);
    let _ = set_foreground(hwnd);
    let deadline = Instant::now() + app::FOCUS_SETTLE;
    loop {
        let verdict = app::focus_verdict(hwnd, foreground());
        if matches!(verdict, FocusVerdict::Confirmed { .. }) || Instant::now() >= deadline {
            return Ok(verdict);
        }
        std::thread::sleep(app::WINDOW_POLL);
    }
}

/// Ask one window to close, then wait for it to stop being a window.
///
/// [`app::close_target_allowed`] runs before anything is posted, so a refusal
/// never reaches the application. The budget comes from the caller: the tool
/// passes [`app::CLOSE_SETTLE`], and a receipt that wants to be sure may pass
/// more.
pub fn close_target(
    target: &WindowInfo,
    our_pid: u32,
    settle: Duration,
) -> Result<CloseVerdict, String> {
    app::close_target_allowed(target, our_pid)?;
    let posted = super::post_close(target.hwnd);
    let deadline = Instant::now() + settle;
    let mut still_here = is_window(target.hwnd);
    while still_here && Instant::now() < deadline {
        std::thread::sleep(app::WINDOW_POLL);
        still_here = is_window(target.hwnd);
    }
    Ok(app::close_verdict(target.hwnd, posted, still_here))
}
