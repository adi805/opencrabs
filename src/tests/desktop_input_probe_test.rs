#![cfg(windows)]
//! Which console shape actually becomes a window on this runner.
//!
//! Two red cycles of guessing were spent on the input receipts. The first blamed
//! a flat frame, the second blamed the capture, and neither was the finding: with
//! the trailing `ping -n 300 127.0.0.1` removed from the marker script, `cmd.exe`
//! was still alive but **no window carrying its title ever appeared in
//! `list_windows()`**, while the console left over from the capture step (which
//! still runs a program) was listed the whole time. The child's prompt showing up
//! in the job log was the other half of that: `D:\a\...>FAILED` is a `cmd` prompt
//! writing into *our* console, which is what happens when the child never got one.
//!
//! So this probe measures instead of theorising. For each candidate shape it
//! reports: is the child still alive, how many windows exist, does any carry the
//! marker, which window is the foreground one, and whether that foreground window
//! is ours. It never asserts, because an assertion here would stop the run at the
//! first variant and throw away the comparison, which is the only thing this file
//! is for.
//!
//! Its output is the artifact `target/input-dump/probe.txt`. Read that before
//! changing anything in `desktop_input_util::spawn_marker_window`.
//!
//! Run:
//! `cargo test --locked --profile ci --target x86_64-pc-windows-msvc --lib \
//!  input_probe -- --ignored --nocapture --test-threads=1`

use std::os::windows::process::CommandExt;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use crate::desktop::interactive_session;
use crate::tests::desktop_input_util::{CREATE_NEW_CONSOLE, note, window_table};

/// One line per candidate console shape.
///
/// `tail` is what the script does last. The shapes differ only in that, and in
/// whether a program is running, which is the variable the two red cycles point
/// at: a `cmd /K` that has finished its commands and sits at a prompt behaves
/// differently from one that is running `ping`, and the difference is visible in
/// whether Windows gives it a window at all.
struct Shape {
    name: &'static str,
    script: &'static str,
    /// How long to let it run before taking the reading.
    settle: Duration,
    /// Whether to hand the child null standard handles instead of ours.
    ///
    /// This is the variable the second red cycle points at and it is easy to
    /// miss: the job log shows the child's `echo` landing *inside our* output
    /// (`test ... OCIN-TYPE-5676` is cmd's stdout going down the inherited
    /// pipe). A console whose standard handles are pipes paints nothing on
    /// screen, so a window that exists, is titled, and is typed into can still
    /// produce a perfectly flat frame, because everything the shell "printed"
    /// went to somebody else's file descriptor.
    null_stdio: bool,
    /// Whether the console is opened through `start`, and how its title slot is
    /// written.
    via_start: ViaStart,
}

/// `start`'s grammar is `start ["title"] program [args...]`, and it decides by
/// position, so the three ways of writing the first slot are three different
/// programs. Measured on the runner, and the measurement corrected two of the
/// expectations written here:
///
/// * `Bare` puts a word Windows then tries to *run*. It answers with a `#32770`
///   dialog and the shell never starts.
/// * `Quoted` was expected to be the console. It is not: the quote characters
///   are escaped by the standard library for `CommandLineToArgvW`, which `cmd`
///   does not speak, so `start` saw a token beginning with a backslash and took
///   it as the program, dialog again. The probe recorded it as `marker_found=true`
///   because the dialog's caption contained the marker: a title-only match is
///   satisfied by the operating system complaining at us, which is why
///   `wait_for_window` now insists on the window class as well.
/// * `NoTitle` is what the receipts ship: no first slot at all, so `start` runs
///   the first unquoted word (`cmd.exe`) and the console's own `title` command
///   puts the marker in the caption.
#[derive(PartialEq)]
enum ViaStart {
    No,
    /// Title slot written with quote characters (the shape that looked right).
    Quoted,
    /// No title slot: `start` takes the first unquoted word as the program.
    NoTitle,
    /// A word in the title slot with nothing quoting it, so it is run instead.
    Bare,
}

const SHAPES: &[Shape] = &[
    // The shape the receipts ship, first so a truncated run still reports it.
    // Expectation, written down before the run: `start-no-title` is the only one
    // of the three `start` shapes that reaches a prompt, because it is the only
    // one that does not hand `start` a token to misinterpret. If it comes back
    // `EXITED` while `running-ping` is alive, the marker never made it into the
    // caption and the `title` command inside the script is what to blame, not
    // `start`. If it is alive but `marker_found=false`, then `list_windows` is
    // not seeing the window at all, which is the tab-merging hazard.
    Shape {
        name: "start-no-title",
        script: "title {m} & mode con cols=100 lines=20 & echo {m}",
        settle: Duration::from_millis(3_000),
        null_stdio: false,
        via_start: ViaStart::NoTitle,
    },
    // The two shapes that answer the question the previous probe never asked: it
    // spawned `cmd.exe` directly in all five cases, so the `start` route the
    // receipts ship was never on the table.
    Shape {
        name: "start-quoted-title",
        script: "title {m} & mode con cols=100 lines=20 & echo {m}",
        settle: Duration::from_millis(3_000),
        null_stdio: false,
        via_start: ViaStart::Quoted,
    },
    Shape {
        name: "start-bare-title",
        script: "title {m} & mode con cols=100 lines=20 & echo {m}",
        settle: Duration::from_millis(3_000),
        null_stdio: false,
        via_start: ViaStart::Bare,
    },
    Shape {
        name: "idle-prompt",
        script: "title {m} & mode con cols=100 lines=20 & echo {m}",
        settle: Duration::from_millis(2_000),
        null_stdio: false,
        via_start: ViaStart::No,
    },
    Shape {
        name: "running-ping",
        script: "title {m} & mode con cols=100 lines=20 & echo {m} & ping -n 300 127.0.0.1 > nul",
        settle: Duration::from_millis(2_000),
        null_stdio: false,
        via_start: ViaStart::No,
    },
    Shape {
        name: "prompt-then-ping",
        script: "title {m} & mode con cols=100 lines=20 & echo {m} & ping -n 6 127.0.0.1 > nul",
        settle: Duration::from_millis(2_000),
        null_stdio: false,
        via_start: ViaStart::No,
    },
    Shape {
        name: "idle-null-stdio",
        script: "title {m} & mode con cols=100 lines=20 & echo {m}",
        settle: Duration::from_millis(2_000),
        null_stdio: true,
        via_start: ViaStart::No,
    },
    Shape {
        name: "no-new-console",
        script: "title {m} & echo {m}",
        settle: Duration::from_millis(2_000),
        null_stdio: false,
        via_start: ViaStart::No,
    },
];

/// The reading for one shape, in the order the questions matter.
fn describe(shape: &Shape, marker: &str, fresh_console: bool) -> String {
    let script = shape.script.replace("{m}", marker);
    // Only the two shapes with a title slot read this; `NoTitle` has none.
    let title = match shape.via_start {
        ViaStart::Quoted => format!("\"{marker}\""),
        _ => marker.to_string(),
    };
    let mut spawn = Command::new("cmd.exe");
    match shape.via_start {
        ViaStart::No => {
            spawn.args(["/K", script.as_str()]);
        }
        ViaStart::NoTitle => {
            spawn.args(["/C", "start", "cmd.exe", "/K", script.as_str()]);
        }
        ViaStart::Quoted | ViaStart::Bare => {
            spawn.args([
                "/C",
                "start",
                title.as_str(),
                "cmd.exe",
                "/K",
                script.as_str(),
            ]);
        }
    }
    if shape.null_stdio {
        spawn
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
    }
    if fresh_console {
        spawn.creation_flags(CREATE_NEW_CONSOLE);
    }
    let mut child: Child = match spawn.spawn() {
        Ok(child) => child,
        Err(why) => return format!("{marker}: spawn failed: {why}"),
    };

    // Poll until the marker shows up or the settle window closes, so a shape that
    // is merely slow is not recorded as a shape that never appears.
    let deadline = Instant::now() + shape.settle;
    let mut found: Option<crate::desktop::WindowInfo> = None;
    while Instant::now() < deadline {
        if let Ok(list) = crate::desktop::list_windows() {
            found = list
                .windows
                .iter()
                .find(|w| w.title.contains(marker))
                .cloned();
            if found.is_some() {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let alive = match child.try_wait() {
        Ok(None) => "alive",
        Ok(Some(_)) => "EXITED",
        Err(why) => return format!("{marker}: try_wait failed: {why}"),
    };

    let listed = crate::desktop::list_windows()
        .map(|list| {
            let count = list.windows.len();
            let foreground = list
                .windows
                .iter()
                .find(|w| w.foreground)
                .map(|w| format!("hwnd={} {:?}", w.hwnd, w.title))
                .unwrap_or_else(|| String::from("(no listed window claims focus)"));
            format!("{count} windows, foreground={foreground}")
        })
        .unwrap_or_else(|why| format!("enumeration failed: {why}"));

    // What the match actually landed on. The class is the half that matters: a
    // #32770 error dialog carries the marker in its caption too, so a bare
    // "found" is not enough to tell a console from a complaint.
    let what = match &found {
        Some(w) => format!(
            "marker_found=true class={:?} console={} hwnd={}",
            w.class,
            crate::desktop::app::is_console_host(&w.class),
            w.hwnd
        ),
        None => String::from("marker_found=false"),
    };
    let report = format!(
        "[probe] {}: fresh_console={} child={alive} {what} {listed}",
        shape.name, fresh_console,
    );
    let _ = child.kill();
    let _ = child.wait();
    report
}

/// The single job this file has.
#[test]
#[ignore = "diagnostic; run by hand against a real runner to read the table"]
fn input_dump_probe_which_console_shape_becomes_a_window() {
    if !interactive_session() {
        note("no interactive seat on this host, so there is nothing to probe");
        return;
    }
    let pid = std::process::id();
    let mut lines = Vec::new();
    for shape in SHAPES {
        let marker = format!("OCPR-{}-{}", shape.name, pid);
        // The fourth shape is the control without `CREATE_NEW_CONSOLE`; the rest
        // are the real candidates.
        let fresh = shape.name != "no-new-console";
        let line = describe(shape, &marker, fresh);
        note(&line);
        lines.push(line);
        std::thread::sleep(Duration::from_millis(500));
    }
    lines.push(String::new());
    lines.push("[probe] windows at the end of the run:".to_string());
    lines.push(window_table());
    let body = lines.join("\n") + "\n";
    let path = crate::tests::desktop_input_util::out_dir().join("probe.txt");
    if let Err(why) = std::fs::write(&path, &body) {
        note(&format!("could not write {}: {why}", path.display()));
    }
    // Deliberate: no assertion. This run is the measurement, and passing or
    // failing it says nothing about the question it asks.
}
