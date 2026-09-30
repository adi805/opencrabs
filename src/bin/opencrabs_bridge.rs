// FORK-ONLY: Windows stdio bridge shim (opencrabs-bridge.exe) for CrabsCode/Zed ACP over SSH.
// Replaces the old hermes.exe slot-hack: a real .exe keeps cmd.exe out of the
// spawn chain, so piped stdin (JSON-RPC frames) reaches ssh byte-for-byte.
// Gate shape: stem "opencrabs-bridge" is in CrabsCode's accepted list for the
// opencrabs provider, and --version answers locally with an "opencrabs" banner.
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(|a| a.eq_ignore_ascii_case("--version")).unwrap_or(false) {
        println!("opencrabs-acp-bridge 0.5.4");
        return;
    }
    // Diagnostics: append every bridge lifecycle event to %USERPROFILE%\opencrabs-bridge.log
    // so GUI-context failures (Zed/MonoCode spawn) are visible without a console.
    fn blog(msg: &str) {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if let Ok(home) = std::env::var("USERPROFILE") {
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(format!("{home}\\opencrabs-bridge.log"))
            {
                use std::io::Write;
                let _ = writeln!(f, "[{t}] {msg}");
            }
        }
    }
    blog(&format!("START args={args:?}"));
    // Absolute ssh path first: GUI-spawned processes may get a sanitized PATH.
    let ssh = if std::path::Path::new("C:\\Windows\\System32\\OpenSSH\\ssh.exe").exists() {
        "C:\\Windows\\System32\\OpenSSH\\ssh.exe"
    } else {
        "ssh"
    };
    // status() inherits stdin/stdout/stderr by default: ssh talks straight to
    // the editor's pipes, byte-for-byte, no cmd.exe in between.
    let st = std::process::Command::new(ssh)
        .args([
            "-o", "BatchMode=yes",
            "-o", "ConnectTimeout=10",
            "-o", "ServerAliveInterval=15",
            "joyboy",
            "/home/agentadmin/.opencrabs/opencrabs",
        ])
        .args(&args)
        .status();
    match st {
        Ok(s) => {
            let c = s.code().unwrap_or(1);
            blog(&format!("EXIT={c}"));
            std::process::exit(c);
        }
        Err(e) => {
            blog(&format!("SPAWN-ERR {e}"));
            eprintln!("opencrabs-bridge: failed to launch ssh: {e}");
            std::process::exit(127);
        }
    }
}
