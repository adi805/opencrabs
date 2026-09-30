// FORK-ONLY: Windows stdio bridge shim (hermes.exe) for MonoCode/Zed ACP over SSH.
// Replaces hermes.cmd: a real .exe keeps cmd.exe out of the spawn chain, so
// piped stdin (JSON-RPC frames) reaches ssh intact. Gate-friendly: file stem
// is "hermes" and `--version` answers locally with a hermes-shaped semver.
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(|a| a.eq_ignore_ascii_case("--version")).unwrap_or(false) {
        println!("hermes 0.5.4 opencrabs-acp-bridge");
        return;
    }
    // status() inherits stdin/stdout/stderr by default: ssh talks straight to
    // the editor's pipes, byte-for-byte, no cmd.exe in between.
    let st = std::process::Command::new("ssh")
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
        Ok(s) => std::process::exit(s.code().unwrap_or(1)),
        Err(e) => {
            eprintln!("hermes bridge: failed to launch ssh: {e}");
            std::process::exit(127);
        }
    }
}
