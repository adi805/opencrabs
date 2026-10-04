//! Tests for the Windows Scheduled Task script builders
//! (`crate::cli::service_windows`).
//!
//! Extracted from an inline `#[cfg(test)]` block in `src/cli/service_windows.rs`;
//! project policy (CONTRIBUTING.md, "Where Tests Live") requires every test to
//! live under `src/tests/`, and this directory has 14 files that say they came
//! from an inline block, so the series direction is extraction, not the reverse.
//!
//! Not gated on `cfg(windows)` as a whole, and that is deliberate: the module
//! compiles on every platform because the builders are pure text, so 10 of the
//! 11 assertions here run in the Linux `Test` job. The two that describe a
//! platform-specific answer are gated individually, with the reason written
//! where the gate is.

use std::path::PathBuf;

#[cfg(windows)]
use crate::cli::service_windows::shell_program;
#[cfg(not(windows))]
use crate::cli::service_windows::{TaskResult, run_script};
use crate::cli::service_windows::{
    install_script, join_arguments, parse_status, ps_str, stop_and_wait_script, stop_script,
    task_name, uninstall_script,
};

#[test]
fn task_name_tracks_the_launchd_identifier() {
    assert_eq!(task_name("com.opencrabs.daemon"), "com.opencrabs.daemon");
}

#[test]
fn ps_str_quotes_and_escapes_literals() {
    assert_eq!(
        ps_str(r"C:\Users\joe\opencrabs.exe"),
        r"'C:\Users\joe\opencrabs.exe'"
    );
    assert_eq!(ps_str("it's"), "'it''s'");
    assert_eq!(ps_str("$env:x"), "'$env:x'");
    assert_eq!(ps_str("a; Remove-Item *"), "'a; Remove-Item *'");
}

#[test]
fn join_arguments_quotes_only_what_needs_it() {
    assert_eq!(join_arguments(&["daemon".into()]), "daemon");
    assert_eq!(
        join_arguments(&["-p".into(), "work".into(), "daemon".into()]),
        "-p work daemon"
    );
    assert_eq!(
        join_arguments(&["-p".into(), "es cap e".into(), "daemon".into()]),
        "-p 'es cap e' daemon"
    );
}

/// The apostrophe-in-path case that would otherwise break PS parsing
/// and inject script: it must arrive doubled inside the literal.
#[test]
fn install_script_survives_hostile_paths() {
    let s = install_script(
        "com.opencrabs.daemon",
        &PathBuf::from(r"C:\Users\O'Brien\opencrabs.exe"),
        &["daemon".into()],
        "don't; do this",
    );
    assert!(
        s.contains(r#"-Execute 'C:\Users\O''Brien\opencrabs.exe'"#),
        "{s}"
    );
    assert!(s.contains(r#"-Description 'don''t; do this'"#), "{s}");
    assert!(s.contains("-ExecutionTimeLimit ([TimeSpan]::Zero)"));
    assert!(s.contains("-MultipleInstances IgnoreNew"));
    assert!(s.contains("-Force"));
}

#[test]
fn status_parses_line_protocol() {
    let raw = "state|Running\nenabled|True\naction|C:\\a\\opencrabs.exe daemon\nlastrun\nnextrun|1/1/2026 1:00 AM\nresult|0";
    let st = parse_status(raw).unwrap();
    assert_eq!(st.state, "Running");
    assert!(st.enabled);
    assert_eq!(st.next_run, "1/1/2026 1:00 AM");
    // A line missing its value separator still yields the field default.
    assert_eq!(st.last_run, "-");
    assert_eq!(st.last_result, "0");
}

#[test]
fn status_rejects_foreign_output() {
    assert!(parse_status("garbage without protocol").is_err());
}

#[test]
fn lifecycle_scripts_target_the_task_by_literal() {
    assert!(uninstall_script("t").contains("-Confirm:$false"));
    assert!(stop_script("t").contains("Stop-ScheduledTask"));
    // quoting must survive a hostile task name in the wait loop too
    let w = stop_and_wait_script("t'x");
    assert!(w.contains("Stop-ScheduledTask -TaskName 't''x'"));
    assert!(w.contains("Get-ScheduledTask -TaskName 't''x'"));
    assert!(w.contains("exit 1"));
}

#[test]
fn install_settings_restart_crashes_and_never_expire() {
    let s = install_script("t", std::path::Path::new("C:\\bin\\oc.exe"), &[], "d");
    assert!(s.contains("-RestartCount 3"));
    assert!(s.contains("-RestartInterval (New-TimeSpan -Minutes 1)"));
    assert!(s.contains("-ExecutionTimeLimit ([TimeSpan]::Zero)"));
}

/// Windows-only, and the reason it is gated rather than adapted.
///
/// Off Windows the module still compiles (the builders are pure text), but
/// `shell_program`'s answer is a `C:\Windows\...` path and a Unix host reads
/// that two ways, both of which break this test: `Path::is_absolute()` is false,
/// and `\` is not a separator so `file_name()` returns the whole string.
/// Asserting either there fails for the wrong reason and takes the Linux `Test`
/// job down with it.
///
/// Where it does run is stated exactly, because "somewhere in CI" is not a
/// guarantee: it runs in the Windows slice of `ci.yml`, which is the job that
/// exists to run Windows-only assertions.
#[cfg(windows)]
#[test]
fn shell_program_is_an_absolute_trusted_path() {
    let p = shell_program();
    assert!(p.is_absolute(), "must never be a bare PATH name: {p:?}");
    let leaf = p.file_name().map(|f| f.to_string_lossy().into_owned());
    assert!(matches!(
        leaf.as_deref(),
        Some("pwsh.exe") | Some("powershell.exe")
    ));
}

/// Off Windows only: the point is that the process layer refuses rather than
/// running a script. On Windows the same call is covered by the artifact
/// workflow, which runs the verbs for real.
#[cfg(not(windows))]
#[test]
fn run_script_is_inert_off_windows() {
    assert!(matches!(run_script("x"), TaskResult::Failed(_)));
}

/// The wait loop must distinguish "the task is gone" from "I could not ask".
/// Exit 0 means stopped to the caller, so every unclassifiable query error
/// has to land on a different code -- otherwise `service uninstall`
/// unregisters a task whose daemon is still holding the instance lock, and
/// nothing can address that process afterwards.
#[test]
fn stop_and_wait_treats_only_a_missing_task_as_stopped() {
    let s = stop_and_wait_script("com.opencrabs.daemon");
    assert!(
        s.contains("$_.CategoryInfo.Category -eq 'ObjectNotFound'"),
        "the not-found case must be discriminated explicitly: {s}"
    );
    // The not-found branch exits 0; the catch-all must not.
    let catch_all = s
        .split("[Console]::Error.WriteLine")
        .nth(1)
        .expect("catch-all branch present");
    assert!(
        catch_all.contains("exit 2"),
        "an unclassifiable query failure must not exit 0: {s}"
    );
    assert!(
        s.contains("if ((Get-Date) -gt $deadline) { exit 1 }"),
        "the timeout path exits 1 so callers can refuse to proceed: {s}"
    );
    // The task name is a single-quoted literal, never interpolated raw.
    assert!(s.contains("-TaskName 'com.opencrabs.daemon'"), "{s}");
}
