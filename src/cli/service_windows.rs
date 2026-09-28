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
/// exit itself, not the signal. A vanished task counts as stopped. Any OTHER
/// query failure must not, though: it exits 2 so `run_script` reports Failed
/// rather than Missing, because an error we cannot classify is not evidence
/// that the daemon stopped -- and `uninstall` would unregister a task whose
/// process is still holding the instance lock.
pub fn stop_and_wait_script(task: &str) -> String {
    format!(
        "$ErrorActionPreference='Stop'; Stop-ScheduledTask -TaskName {t} -ErrorAction SilentlyContinue; \
         $deadline=(Get-Date).AddSeconds(30); \
         while ($true) {{ \
           try {{ $t = Get-ScheduledTask -TaskName {t} -ErrorAction Stop }} \
           catch {{ \
             if ($_.CategoryInfo.Category -eq 'ObjectNotFound') {{ exit 0 }} \
             [Console]::Error.WriteLine('cannot query task state: ' + $_.Exception.Message); exit 2 \
           }} \
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
/// `pub(crate)` rather than private only because the extracted test in
/// `crate::tests::service_windows_test` reaches it: every other item this module
/// exports is part of the CLI surface. Same shape as `norm_key` in the Discord
/// handler, which the repo widened when its own inline block was extracted.
pub(crate) fn shell_program() -> std::path::PathBuf {
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
