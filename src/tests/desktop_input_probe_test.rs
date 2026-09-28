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
}

const SHAPES: &[Shape] = &[
    Shape {
        name: "idle-prompt",
        script: "title {m} & mode con cols=100 lines=20 & echo {m}",
        settle: Duration::from_millis(2_000),
        null_stdio: false,
    },
    Shape {
        name: "running-ping",
        script: "title {m} & mode con cols=100 lines=20 & echo {m} & ping -n 300 127.0.0.1 > nul",
        settle: Duration::from_millis(2_000),
        null_stdio: false,
    },
    Shape {
        name: "prompt-then-ping",
        script: "title {m} & mode con cols=100 lines=20 & echo {m} & ping -n 6 127.0.0.1 > nul",
        settle: Duration::from_millis(2_000),
        null_stdio: false,
    },
    Shape {
        name: "idle-null-stdio",
        script: "title {m} & mode con cols=100 lines=20 & echo {m}",
        settle: Duration::from_millis(2_000),
        null_stdio: true,
    },
    Shape {
        name: "no-new-console",
        script: "title {m} & echo {m}",
        settle: Duration::from_millis(2_000),
        null_stdio: false,
    },
];

/// The reading for one shape, in the order the questions matter.
fn describe(shape: &Shape, marker: &str, fresh_console: bool) -> String {
    let script = shape.script.replace("{m}", marker);
    let mut spawn = Command::new("cmd.exe");
    spawn.args(["/K", script.as_str()]);
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
    let mut found = false;
    while Instant::now() < deadline {
        if let Ok(list) = crate::desktop::list_windows() {
            found = list.windows.iter().any(|w| w.title.contains(marker));
            if found {
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

    let report = format!(
        "[probe] {}: fresh_console={} child={alive} marker_found={found} {listed}",
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
