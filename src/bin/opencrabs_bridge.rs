// FORK-ONLY: Windows stdio bridge shim (opencrabs-bridge.exe) for CrabsCode/Zed ACP over SSH.
// v2: explicit piped pump instead of handle inheritance. A console child (ssh.exe)
// spawned from a GUI-parent chain on Windows may rebind its std handles to a fresh
// console, so frames never reach ssh even though EOF (handle close) propagates.
// v2 spawns ssh with explicit piped stdio and pumps bytes across threads, logging
// byte counts to opencrabs-bridge.log: STDIN-FIRST / STDOUT-FIRST / SSHERR-FIRST /
// EOF lines + EXIT totals. One GUI attempt then proves exactly where frames die.
// --selftest round-trips a marker through the same pump+ssh without any editor.
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(|a| a.eq_ignore_ascii_case("--version")).unwrap_or(false) {
        println!("opencrabs-acp-bridge 0.5.4");
        return;
    }
    if args.first().map(|a| a.eq_ignore_ascii_case("--selftest")).unwrap_or(false) {
        selftest();
        return;
    }
    blog(&format!("START args={args:?}"));
    let mut child = match ssh_cmd(&args).spawn() {
        Ok(c) => c,
        Err(e) => {
            blog(&format!("SPAWN-ERR {e}"));
            eprintln!("opencrabs-bridge: failed to launch ssh: {e}");
            std::process::exit(127);
        }
    };
    let child_in = child.stdin.take();
    let child_out = child.stdout.take();
    let child_err = child.stderr.take();
    let n_in = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let n_out = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let n_err = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let t_in = std::thread::spawn(move || {
        if let Some(w) = child_in {
            pump(std::io::stdin(), w, &n_in, "STDIN");
        }
    });
    let t_out = std::thread::spawn(move || {
        if let Some(r) = child_out {
            pump(r, std::io::stdout(), &n_out, "STDOUT");
        }
    });
    let t_err = std::thread::spawn(move || {
        if let Some(r) = child_err {
            pump(r, std::io::stderr(), &n_err, "SSHERR");
        }
    });
    let st = child.wait();
    // ssh died: its stdout/stderr pipes hit EOF, so those pumps finish fast.
    // The stdin pump may linger on the app's still-open pipe: detach it.
    let _ = t_out.join();
    let _ = t_err.join();
    let i = n_in.load(std::sync::atomic::Ordering::Relaxed);
    let o = n_out.load(std::sync::atomic::Ordering::Relaxed);
    let e = n_err.load(std::sync::atomic::Ordering::Relaxed);
    let code = st.ok().and_then(|s| s.code()).unwrap_or(1);
    blog(&format!("EXIT={code} in={i}B out={o}B err={e}B"));
    std::process::exit(code);
}

fn ssh_cmd(args: &[String]) -> std::process::Command {
    // Absolute ssh path first: GUI-spawned processes may get a sanitized PATH.
    let ssh = if std::path::Path::new("C:\\Windows\\System32\\OpenSSH\\ssh.exe").exists() {
        "C:\\Windows\\System32\\OpenSSH\\ssh.exe"
    } else {
        "ssh"
    };
    let mut cmd = std::process::Command::new(ssh);
    cmd.args([
        "-o", "BatchMode=yes",
        "-o", "ConnectTimeout=10",
        "-o", "ServerAliveInterval=15",
        "joyboy",
        "/home/agentadmin/.opencrabs/opencrabs",
    ]);
    cmd.args(args);
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    cmd
}

fn pump<R: std::io::Read, W: std::io::Write>(
    mut r: R,
    mut w: W,
    n: &std::sync::atomic::AtomicUsize,
    label: &'static str,
) {
    use std::io::{Read, Write};
    let mut buf = [0u8; 8192];
    let mut first = true;
    let mut total = 0usize;
    loop {
        match r.read(&mut buf) {
            Ok(0) => {
                blog(&format!("{label}-EOF total={total}B"));
                let _ = w.flush();
                return;
            }
            Ok(k) => {
                total += k;
                n.fetch_add(k, std::sync::atomic::Ordering::Relaxed);
                if first {
                    first = false;
                    blog(&format!("{label}-FIRST {k}B"));
                }
                if w.write_all(&buf[..k]).is_err() {
                    blog(&format!("{label}-WRITE-ERR at total={total}B"));
                    return;
                }
                let _ = w.flush();
            }
            Err(_) => {
                blog(&format!("{label}-READ-ERR at total={total}B"));
                return;
            }
        }
    }
}

fn selftest() {
    blog("SELFTEST start");
    let mut cmd = std::process::Command::new(ssh_path());
    cmd.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "joyboy", "printf", "bridge-selftest-ok"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            blog(&format!("SELFTEST SPAWN-ERR {e}"));
            println!("SELFTEST FAIL: spawn ssh: {e}");
            std::process::exit(1);
        }
    };
    drop(child.stdin.take());
    let mut out = String::new();
    if let Some(mut r) = child.stdout.take() {
        let _ = r.read_to_string(&mut out);
    }
    let mut err = String::new();
    if let Some(mut r) = child.stderr.take() {
        let _ = r.read_to_string(&mut err);
    }
    let st = child.wait();
    let ok = out.contains("bridge-selftest-ok");
    blog(&format!(
        "SELFTEST done ok={ok} out={}B err={}B code={:?}",
        out.len(),
        err.len(),
        st.ok().and_then(|s| s.code())
    ));
    if ok {
        println!("SELFTEST PASS: ssh round-trip through pump works");
    } else {
        println!("SELFTEST FAIL: out={out:?} err={err:?}");
        std::process::exit(1);
    }
}

fn ssh_path() -> &'static str {
    if std::path::Path::new("C:\\Windows\\System32\\OpenSSH\\ssh.exe").exists() {
        "C:\\Windows\\System32\\OpenSSH\\ssh.exe"
    } else {
        "ssh"
    }
}

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
