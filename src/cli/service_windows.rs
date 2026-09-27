//! Per-profile Scheduled Task autostart for Windows.
//!
//! Why a Scheduled Task and not a Windows Service: a service needs
//! elevation to install and is bound to machine boot, but the ask here is
//! "run when this user logs in, do not run when they do not". An
//! interactive logon trigger with a user-scoped action is the honest form
//! of that and needs no admin. This is the call both Hermes and OpenClaw
//! landed on independently.
//!
//! Two deliberate deviations from those designs, recorded here so the next
//! reader does not "fix" them back:
//!
//! * The task action is the daemon binary directly, NOT a `wscript` /
//!   `powershell` wrapper. A wrapper buys a hidden console window but
//!   breaks `Stop-ScheduledTask`: the scheduler kills the wrapper and the
//!   daemon keeps running as an orphan behind it, so stop/status lie.
//!   A visible console is the honest failure mode of the two.
//! * Every script here is one single-quoted PowerShell literal per
//!   caller-supplied string (see [`ps_str`]); nothing interpolates raw,
//!   so path characters cannot escape a string boundary.
//!
//! The builders are pure functions returning `String`, and the parser is
//! pure, so all of it is unit-tested on any host OS; only `run()` touches
//! the process layer, and it is inert off Windows.

use std::path::Path;

/// The task name IS the launchd identifier (`com.opencrabs.daemon[.p]`),
/// so one profile has one human-recognizable identity across platforms.
pub fn task_name(plist_name: &str) -> String {
    plist_name.to_string()
}

/// Escape for the inside of a PowerShell single-quoted literal: the only
/// escape in that form is doubling the quote character.
pub fn ps_str(value: impl AsRef<str>) -> String {
    format!("'{}'", value.as_ref().replace('\'', "''"))
}

/// Join the daemon argument vector for `-Argument`. Tokens are the CLI's
/// own (profile slug + `daemon`); quote any token that gained whitespace
/// by wrapping it in single quotes with the same literal escaping.
pub fn join_arguments(args: &[String]) -> String {
    args.iter()
        .map(|a| {
            if a.chars().any(|c| c.is_whitespace()) {
                ps_str(a)
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Register (or re-register with `-Force`) the logon task. Re-registering
/// is the supported way to follow a moved binary, so install is idempotent
/// by design.
///
/// Settings: `-StartWhenAvailable` catches a missed logon (machine asleep,
/// laptop lid); `-ExecutionTimeLimit Zero` stops the scheduler killing a
/// long-running daemon at its default 72h; `-MultipleInstances IgnoreNew`
/// stops a second logon stacking daemons — the instance lock (#3) is the
/// backstop, but the task layer should not be racing it on purpose.
pub fn install_script(task: &str, binary: &Path, args: &[String], description: &str) -> String {
    format!(
        "$ErrorActionPreference='Stop'; \
         $a = New-ScheduledTaskAction -Execute {exe} -Argument {arg}; \
         $t = New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME; \
         $s = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries \
         -StartWhenAvailable -MultipleInstances IgnoreNew -ExecutionTimeLimit ([TimeSpan]::Zero); \
         Register-ScheduledTask -TaskName {name} -Action $a -Trigger $t -Settings $s \
         -Description {desc} -Force | Out-Null; \
         'ok'",
        exe = ps_str(binary.display().to_string()),
        arg = ps_str(join_arguments(args)),
        name = ps_str(task),
        desc = ps_str(description),
    )
}

pub fn start_script(task: &str) -> String {
    format!(
        "$ErrorActionPreference='Stop'; Start-ScheduledTask -TaskName {}",
        ps_str(task)
    )
}

pub fn stop_script(task: &str) -> String {
    format!(
        "$ErrorActionPreference='Stop'; Stop-ScheduledTask -TaskName {}",
        ps_str(task)
    )
}

/// Arm or disarm the logon trigger without deleting the registration:
/// `disable` is what "keep the binary but stop autostarting" means here,
/// mirroring `systemctl disable` more than `uninstall`.
pub fn enable_script(task: &str) -> String {
    format!(
        "$ErrorActionPreference='Stop'; Enable-ScheduledTask -TaskName {}",
        ps_str(task)
    )
}

pub fn disable_script(task: &str) -> String {
    format!(
        "$ErrorActionPreference='Stop'; Disable-ScheduledTask -TaskName {}",
        ps_str(task)
    )
}

pub fn uninstall_script(task: &str) -> String {
    format!(
        "$ErrorActionPreference='Stop'; Unregister-ScheduledTask -TaskName {} -Confirm:$false; 'ok'",
        ps_str(task)
    )
}

/// One pipe-delimited line per field (never JSON: `ConvertTo-Json`
/// formatting differs across PS 5.1/7, and `-Compress` on 7 still differs
/// in case). `Get-ScheduledTaskInfo` has no `-TaskName`; the task object
/// must be piped into it.
pub fn status_script(task: &str) -> String {
    format!(
        "$ErrorActionPreference='Stop'; \
         $t = Get-ScheduledTask -TaskName {name}; \
         $i = $t | Get-ScheduledTaskInfo; \
         \"state|$($t.State)\"; \
         \"enabled|$($t.Settings.Enabled)\"; \
         \"action|$($t.Actions[0].Execute) $($t.Actions[0].Arguments)\"; \
         \"lastrun|$($i.LastRunTime)\"; \
         \"nextrun|$($i.NextRunTime)\"; \
         \"result|$($i.LastTaskResult)\"",
        name = ps_str(task)
    )
}

/// Parsed [`status_script`] output.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskStatus {
    pub state: String,
    pub enabled: bool,
    pub action: String,
    pub last_run: String,
    pub next_run: String,
    pub last_result: String,
}

/// Tolerant parse: unknown lines are ignored, missing fields default to
/// `-` rather than failing the report (a half-readable status beats an
/// error dialog).
pub fn parse_status(raw: &str) -> Result<TaskStatus, String> {
    let mut get = |key: &str| -> String {
        raw.lines()
            .find_map(|l| l.split_once('|').filter(|(k, _)| *k == key).map(|(_, v)| v.trim().to_string()))
            .unwrap_or_else(|| "-".to_string())
    };
    if !raw.contains("state|") {
        return Err(format!("unreadable task status output: {raw}"));
    }
    Ok(TaskStatus {
        state: get("state"),
        enabled: get("enabled") == "True",
        action: get("action"),
        last_run: get("lastrun"),
        next_run: get("nextrun"),
        last_result: get("result"),
    })
}

/// Locate a PowerShell host: `pwsh` (7+) when present, else the in-box
/// Windows PowerShell. Returns the program to invoke.
fn shell_program() -> &'static str {
    // where.exe is the platform locator; pwsh missing from PATH simply
    // falls through to the in-box shell, which exists by OS guarantee.
    let found = std::process::Command::new("where.exe")
        .arg("pwsh")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if found { "pwsh" } else { "powershell" }
}

/// Outcome of a script run: success with stdout, or a one-line message
/// ready for the CLI.
pub enum TaskResult {
    Ok(String),
    /// The named task is not registered (PS "cannot find" phrasing).
    Missing,
    Failed(String),
}

/// Run one script with the hidden-window creation flag (a console flash
/// per CLI call is the wrapper problem again, and here the child IS the
/// shell we are waiting on, so there is no orphans trade-off).
pub fn run_script(script: &str) -> TaskResult {
    if !cfg!(windows) {
        return TaskResult::Failed("scheduled tasks are a Windows facility".into());
    }
    let mut cmd = std::process::Command::new(shell_program());
    cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    match cmd.output() {
        Ok(out) if out.status.success() => TaskResult::Ok(
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
        ),
        Ok(out) => {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            // PS 5.1 and 7 phrase "no such task" differently; match the
            // common stems rather than full sentences.
            let lower = err.to_lowercase();
            if lower.contains("cannot find") || lower.contains("no task") || lower.contains("not exist") {
                TaskResult::Missing
            } else {
                TaskResult::Failed(if err.is_empty() {
                    format!("powershell exited {}", out.status)
                } else {
                    err
                })
            }
        }
        Err(e) => TaskResult::Failed(format!("could not start the powershell host: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn task_name_tracks_the_launchd_identifier() {
        assert_eq!(task_name("com.opencrabs.daemon"), "com.opencrabs.daemon");
    }

    #[test]
    fn ps_str_quotes_and_escapes_literals() {
        assert_eq!(ps_str(r"C:\Users\joe\opencrabs.exe"), r"'C:\Users\joe\opencrabs.exe'");
        assert_eq!(ps_str("it's"), "'it''s'");
        assert_eq!(ps_str("$env:x"), "'$env:x'");
        assert_eq!(ps_str("a; Remove-Item *"), "'a; Remove-Item *'");
    }

    #[test]
    fn join_arguments_quotes_only_what_needs_it() {
        assert_eq!(join_arguments(&["daemon".into()]), "daemon");
        assert_eq!(join_arguments(&["-p".into(), "work".into(), "daemon".into()]), "-p work daemon");
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
        assert!(s.contains(r#"-Execute 'C:\Users\O''Brien\opencrabs.exe'"#), "{s}");
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
    fn enable_disable_uninstall_target_the_task_by_literal() {
        assert_eq!(enable_script("t'x"), "$ErrorActionPreference='Stop'; Enable-ScheduledTask -TaskName 't''x'");
        assert!(uninstall_script("t").contains("-Confirm:$false"));
        assert!(stop_script("t").contains("Stop-ScheduledTask"));
    }

    #[test]
    fn run_script_is_inert_off_windows() {
        #[cfg(not(windows))]
        assert!(matches!(run_script("x"), TaskResult::Failed(_)));
    }
}
