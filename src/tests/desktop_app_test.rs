//! The app-control decisions: what may be launched, and what counts as done.
//!
//! These run in the ordinary Linux test job. That is the point of putting them
//! in `desktop::app` rather than next to the syscalls: a rule about when we are
//! allowed to claim a window closed can only be trusted if every branch of it
//! has actually been executed, and the only job that executes tests is the Linux
//! one. On Windows these paths are exercised once, on a machine where a mistake
//! closes something real.
//!
//! No test here opens a window, starts a process, or touches a handle. They pass
//! in the numbers the syscalls produce and check what we then say about them.

use crate::desktop::Rect;
use crate::desktop::WindowInfo;
use crate::desktop::app::{
    self, CONSOLE_HOST_CLASSES, CloseVerdict, FocusVerdict, LaunchPlan, MAX_COMMAND_LINE_CHARS,
    MAX_LAUNCH_ARGS,
};

/// A window with just enough shape to be a target. The fields that matter for
/// these rules are `hwnd`, `pid`, and `class`; the rest are filler.
fn window(hwnd: isize, pid: u32, class: &str) -> WindowInfo {
    WindowInfo {
        hwnd,
        pid,
        title: String::from("something"),
        class: class.to_string(),
        rect: Rect {
            left: 0,
            top: 0,
            right: 800,
            bottom: 600,
        },
        foreground: false,
    }
}

#[test]
fn a_dialog_about_our_own_marker_is_not_the_console_we_asked_for() {
    // The measured case this rule exists for: on the CI runner, `start` swallowed
    // a mistyped title argument and asked Windows to run a program that does not
    // exist. Windows answered with a #32770 dialog whose caption named that
    // program, and a test polling for a window containing its own marker string
    // would have been satisfied by the error box. Both console classes are
    // accepted because the runner uses the Windows Terminal one and a plain host
    // uses the other.
    for good in CONSOLE_HOST_CLASSES {
        assert!(app::is_console_host(good), "{good} hosts consoles");
        assert!(
            app::is_console_host(&good.to_ascii_lowercase()),
            "{good} must match regardless of case: the class comes back from Windows as it was registered"
        );
    }
    for not_a_console in ["#32770", "Notepad", "CabinetWClass", ""] {
        assert!(
            !app::is_console_host(not_a_console),
            "{not_a_console:?} is not a console, so it must not satisfy a console receipt"
        );
    }
}

#[test]
fn launch_refuses_what_cannot_survive_one_argv_token() {
    for blank in ["", "   ", "\t"] {
        let err = LaunchPlan::build(blank, &[]).unwrap_err();
        assert!(
            err.contains("empty") || err.contains("needs"),
            "got {err:?}"
        );
    }
    // A quote in a program name is the caller having built a command line. Since
    // nothing here is ever handed to a shell, accepting it would put a quote
    // character inside a literal path and launch the wrong thing.
    let quoted = LaunchPlan::build("\"notepad.exe\"", &[]).unwrap_err();
    assert!(quoted.contains("quote"), "got {quoted:?}");
    let in_arg = LaunchPlan::build("notepad.exe", &[String::from("a\"b")]).unwrap_err();
    assert!(in_arg.contains("quote"), "got {in_arg:?}");

    for (name, bad) in [
        ("nul", String::from("a\0b")),
        ("newline", String::from("a\nb")),
        ("carriage return", String::from("a\rb")),
    ] {
        assert!(
            LaunchPlan::build("notepad.exe", &[bad]).is_err(),
            "an argument with a {name} must be refused, not truncated by the OS"
        );
    }
}

#[test]
fn an_empty_argument_and_a_trailing_backslash_are_kept_not_dropped() {
    // argv can genuinely carry an empty string, and `Command` quotes it so it
    // survives the round trip. Dropping it silently would change the program's
    // behaviour, which is worse than launching nothing.
    let plan = LaunchPlan::build(
        "notepad.exe",
        &[String::new(), String::from("C:\\Users\\Public\\")],
    )
    .expect("both are representable arguments");
    assert_eq!(plan.args.len(), 2, "neither argument may be dropped");
    assert_eq!(plan.args[0], "");
    assert_eq!(plan.args[1], "C:\\Users\\Public\\");
}

#[test]
fn shell_metacharacters_are_inert_so_a_path_with_one_is_allowed() {
    // The tempting filter is to reject `&` and `|`, and it would be wrong twice:
    // they are legal in Windows file names, and there is no shell in this path to
    // interpret them. A path with a space and a bracket is the ordinary case.
    let plan = LaunchPlan::build(
        "C:\\Program Files (x86)\\Tool & Co\\app.exe",
        &[String::from("/out:a|b.txt"), String::from("--log&level=1")],
    )
    .expect("nothing here reaches a shell");
    assert_eq!(plan.args.len(), 2);
}

#[test]
fn too_many_arguments_is_refused_and_says_by_how_much() {
    let many = vec![String::from("x"); MAX_LAUNCH_ARGS + 1];
    let err = LaunchPlan::build("notepad.exe", &many).unwrap_err();
    assert!(err.contains("limit"), "got {err:?}");
    assert!(
        err.contains(&(MAX_LAUNCH_ARGS + 1).to_string()),
        "the message must carry the number it counted: {err:?}"
    );
    assert!(
        LaunchPlan::build("notepad.exe", &vec![String::from("x"); MAX_LAUNCH_ARGS]).is_ok(),
        "exactly at the limit is not over it"
    );
}

#[test]
fn a_command_line_windows_would_truncate_is_refused_instead() {
    // Windows silently cuts the command line at its ceiling; a program started
    // with half an argument list produces symptoms somewhere else entirely.
    let huge = "a".repeat(MAX_COMMAND_LINE_CHARS + 1);
    let err = LaunchPlan::build("notepad.exe", &[huge]).unwrap_err();
    assert!(err.contains("32767"), "got {err:?}");
    assert!(err.contains("command line"), "got {err:?}");
}

#[test]
fn the_program_is_trimmed_because_padding_is_not_part_of_a_name() {
    let plan = LaunchPlan::build("  notepad.exe  ", &[]).expect("trimming is not a refusal");
    assert_eq!(plan.program, "notepad.exe");
}

#[test]
fn the_description_shows_the_program_and_priced_arguments() {
    let bare = LaunchPlan::build("notepad.exe", &[]).unwrap();
    assert_eq!(bare.describe(), "starting \"notepad.exe\"");

    let some = LaunchPlan::build(
        "git.exe",
        &[
            String::from("commit"),
            String::from("-m"),
            String::from("fix it"),
        ],
    )
    .unwrap();
    let line = some.describe();
    assert!(line.contains("git.exe"), "got {line:?}");
    assert!(line.contains("3 argument(s)"), "got {line:?}");
    assert!(line.contains("\"fix it\""), "got {line:?}");

    // More arguments than we show: the count and the remainder must still be
    // visible, or an approver reads a short list that was never short.
    let many = LaunchPlan::build("app.exe", &vec![String::from("arg"); MAX_LAUNCH_ARGS]).unwrap();
    let line = many.describe();
    assert!(line.contains("and 26 more"), "got {line:?}");
    assert!(
        line.chars().count() < 400,
        "an approval prompt has to stay readable, got {} chars",
        line.chars().count()
    );
}

#[test]
fn focus_is_confirmed_only_by_the_os_own_answer() {
    assert_eq!(
        app::focus_verdict(12, 12),
        FocusVerdict::Confirmed { hwnd: 12 },
        "the window we asked for must be the one the OS names afterwards"
    );
    assert_eq!(
        app::focus_verdict(12, 9),
        FocusVerdict::HeldByAnother {
            wanted: 12,
            actual: 9
        },
        "another window holding focus is a refusal by Windows, not a success"
    );
    assert_eq!(
        app::focus_verdict(12, 0),
        FocusVerdict::NothingHoldsFocus { wanted: 12 },
        "0 means no window, and must not be read as a handle we failed to move"
    );

    // The wording is part of the contract: a human reads this line in a log.
    let held = app::focus_verdict(12, 9).to_string();
    assert!(held.contains("did not move"), "got {held:?}");
    assert!(held.contains("12") && held.contains("9"), "got {held:?}");
}

#[test]
fn a_close_is_gone_only_when_the_window_stopped_being_listed() {
    assert_eq!(
        app::close_verdict(7, true, false),
        CloseVerdict::Gone { hwnd: 7 }
    );
    assert_eq!(
        app::close_verdict(7, true, true),
        CloseVerdict::StillHere { hwnd: 7 },
        "delivered and still there is the app asking a human something, not our failure"
    );
    // Undeliverable wins over the listing either way: if the handle was already
    // stale, saying "we closed it" would be a lie about an action that never ran.
    assert_eq!(
        app::close_verdict(7, false, true),
        CloseVerdict::NotDelivered { hwnd: 7 }
    );
    assert_eq!(
        app::close_verdict(7, false, false),
        CloseVerdict::NotDelivered { hwnd: 7 }
    );
    let here = app::close_verdict(7, true, true).to_string();
    assert!(here.contains("still open"), "got {here:?}");
    assert!(
        here.contains("save"),
        "the likely reason belongs in the line: {here:?}"
    );
}

#[test]
fn closing_is_refused_before_anything_is_posted() {
    let ours = std::process::id();
    let other = ours + 1;

    // A normal window in someone else's process is the whole point of the action.
    assert!(app::close_target_allowed(&window(5, other, "Notepad"), ours).is_ok());

    for (label, target) in [
        ("handle 0", window(0, other, "Notepad")),
        ("our own process", window(5, ours, "Notepad")),
        ("the desktop", window(5, other, "Progman")),
        ("the taskbar", window(5, other, "Shell_TrayWnd")),
        ("an unreadable owner", window(5, 0, "Notepad")),
    ] {
        let err = app::close_target_allowed(&target, ours).unwrap_err();
        assert!(!err.is_empty(), "{label} was refused with no reason");
        match label {
            "handle 0" => assert!(err.contains("no window"), "{label}: {err:?}"),
            "our own process" => assert!(err.contains("this process"), "{label}: {err:?}"),
            "an unreadable owner" => assert!(err.contains("owner"), "{label}: {err:?}"),
            _ => assert!(
                err.contains("shell")
                    || err.contains("close")
                    || err.contains("furniture")
                    || err.contains("session"),
                "{label}: {err:?}"
            ),
        }
    }
}

#[test]
fn the_console_classes_listed_are_the_two_this_project_measured() {
    // Pinned so that adding a third is a deliberate act: each one is a claim about
    // a real window class, and the receipt that uses this list is only as good as
    // the measurements behind it.
    assert_eq!(
        CONSOLE_HOST_CLASSES,
        &["ConsoleWindowClass", "CASCADIA_HOSTING_WINDOW_CLASS"]
    );
}

/// A window whose owner and size are the only things this file cares about.
fn sized(hwnd: isize, pid: u32, width: i32, height: i32) -> WindowInfo {
    WindowInfo {
        hwnd,
        pid,
        title: String::from("whatever"),
        class: String::from("ConsoleWindowClass"),
        rect: Rect {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        },
        foreground: false,
    }
}

#[test]
fn the_window_of_a_started_process_is_the_biggest_one_it_owns() {
    // A program's first windows are usually the ones a person never sees: a
    // message-only window, a tray host, an IME bridge. Picking the largest is
    // picking the one that was actually painted.
    let ours = vec![
        sized(11, 700, 4, 4),
        sized(12, 700, 1044, 635),
        sized(13, 700, 200, 200),
        sized(14, 701, 1600, 1200),
    ];
    let found = app::window_of_pid(&ours, 700).expect("this pid owns windows");
    assert_eq!(
        found.hwnd, 12,
        "the largest window of the pid we asked about wins, not another process's"
    );
    assert_eq!(
        app::window_of_pid(&ours, 999),
        None,
        "a pid that owns nothing owns nothing"
    );
}

#[test]
fn a_window_we_could_not_measure_is_not_a_target() {
    // Two shapes the enumeration can really produce, and neither is something to
    // act on. The negative-span rect is the one that matters: its area is a
    // positive product of two negative spans, so a rule that reduced a rect to an
    // area before checking it would pick this window as the target. That is the
    // shape this project already shipped once and had to undo.
    let mut windows = vec![
        sized(21, 800, -40, -40),
        sized(0, 800, 300, 300),
        sized(23, 0, 900, 900),
    ];
    assert_eq!(
        app::window_of_pid(&windows, 800),
        None,
        "a garbage rect and a zero handle are not windows to act on"
    );
    windows.push(sized(22, 800, 300, 200));
    assert_eq!(
        app::window_of_pid(&windows, 800).map(|w| w.hwnd),
        Some(22),
        "adding one measurable window makes the process findable again"
    );
    assert_eq!(
        app::window_of_pid(&windows, 0),
        None,
        "0 is what a failed owner query reports, not an owner; treating it as one \
         would match every window whose query failed"
    );
}

#[test]
fn the_poll_interval_is_shorter_than_every_budget_it_polls_inside() {
    // The interval is what turns one reading into a wait. A poll as long as its
    // budget is a single look with extra steps, and the "not yet" that the budget
    // exists to resolve gets reported as the answer instead.
    for (label, budget) in [
        ("launch", app::LAUNCH_WINDOW_SETTLE),
        ("focus", app::FOCUS_SETTLE),
        ("close", app::CLOSE_SETTLE),
    ] {
        assert!(
            app::WINDOW_POLL < budget,
            "{label}: a {:?} poll is not shorter than its {budget:?} budget",
            app::WINDOW_POLL
        );
    }
}
