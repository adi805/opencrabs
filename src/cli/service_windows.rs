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
/// stops a second logon stacking daemons; the instance lock (#3) is the
/// backstop, but the task layer should not be racing it on purpose.
/// `-RestartCount 3 -RestartInterval 1min` restarts a CRASHED daemon:
/// without it the logon trigger alone means "dead until next login",
/// which is not the always-on promise `service status` makes. The
/// interval is only between retry attempts, not a rate limit on runs.
pub fn install_script(task: &str, binary: &Path, args: &[String], description: &str) -> String {
    format!(
        "$ErrorActionPreference='Stop'; \
         $a = New-ScheduledTaskAction -Execute {exe} -Argument {arg}; \
         $t = New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME; \
         $s = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries \
         -StartWhenAvailable -MultipleInstances IgnoreNew -ExecutionTimeLimit ([TimeSpan]::Zero) \
         -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1); \
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

/// Stop AND wait for the action process to actually exit: a bounded poll
/// (30s, 250ms ticks) on the task's State, exit 1 on timeout so callers
/// can refuse to proceed. Stop-ScheduledTask only signals; the daemon's
/// own shutdown (SQLite WAL flush, channel sockets) is asynchronous, and
/// both `restart` (IgnoreNew would reject the new launch while the old
/// one still holds the instance lock) and `uninstall` (an unregistered
/// task cannot be addressed again, orphaning the live process) need the
/// exit itself, not the signal. A vanished task counts as stopped.
pub fn stop_and_wait_script(task: &str) -> String {
    format!(
        "$ErrorActionPreference='Stop'; Stop-ScheduledTask -TaskName {t} -ErrorAction SilentlyContinue; \
         $deadline=(Get-Date).AddSeconds(30); \
         while ($true) {{ \
           try {{ $t = Get-ScheduledTask -TaskName {t} -ErrorAction Stop }} \
           catch {{ exit 0 }} \
           if ($t.State -ne 'Running') {{ exit 0 }} \
           if ((Get-Date) -gt $deadline) {{ exit 1 }} \
           Start-Sleep -Milliseconds 250 \
         }}",
        t = ps_str(task)
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
    let get = |key: &str| -> String {
        raw.lines()
            .find_map(|l| {
                l.split_once('|')
                    .filter(|(k, _)| *k == key)
                    .map(|(_, v)| v.trim().to_string())
            })
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

/// Locate a PowerShell host from TRUSTED ABSOLUTE paths only.
///
/// Launching the bare names `where.exe`/`pwsh`/`powershell` meant the
/// caller's PATH (and any relative entry in it) chose which binary ran
/// with the user's privileges on every service verb: a planted
/// `powershell.exe` in the current directory was enough. So there is no
/// lookup spawn at all any more: prefer the machine-wide MSI location
/// for pwsh 7, else the in-box Windows PowerShell under System32. Both
/// candidates sit under package-manager ACLs a plain user cannot swap
/// a binary into. PATH-discovered pwsh installs lose here on purpose;
/// every cmdlet this module emits exists in 5.1.
fn shell_program() -> std::path::PathBuf {
    let sys_root = std::env::var("SystemRoot")
        .or_else(|_| std::env::var("windir"))
        .unwrap_or_else(|_| r"C:\Windows".to_string());
    if let Ok(pf) = std::env::var("ProgramFiles") {
        let pwsh7 = std::path::Path::new(&pf)
            .join("PowerShell")
            .join("7")
            .join("pwsh.exe");
        if pwsh7.is_file() {
            return pwsh7;
        }
    }
    std::path::Path::new(&sys_root)
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe")
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
    cmd.args([
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-Command",
        script,
    ])
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
        Ok(out) if out.status.success() => {
            TaskResult::Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        }
        Ok(out) => {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            // PS 5.1 and 7 phrase "no such task" differently; match the
            // common stems rather than full sentences.
            let lower = err.to_lowercase();
            if lower.contains("cannot find")
                || lower.contains("no task")
                || lower.contains("not exist")
            {
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

    #[test]
    fn run_script_is_inert_off_windows() {
        #[cfg(not(windows))]
        assert!(matches!(run_script("x"), TaskResult::Failed(_)));
    }
}
