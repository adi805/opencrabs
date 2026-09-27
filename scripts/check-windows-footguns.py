#!/usr/bin/env python3
r"""Fail CI on Windows footguns in the Rust tree.

A pattern is reported when it is live for the Windows build: outside any
cfg scope, inside a cfg(windows) scope, or inside a scope whose cfg also
matches windows. cfg(unix)-only items, blocks, modules (gates at the
`mod x;` declaration count: that is how the #1759 fix landed) and files
are clean by definition. The analysis is lexical: brace-depth stack,
attribute-aware, comment- and string-split line scanner, no macro
expansion. It exists to stop the #1759 shape (compiles on the platforms
CI measures, dies on the one users have) from silently recurring, and to
catch runtime-only POSIX spawns that even a green windows build cannot
see. It is not a substitute for the windows test slice.

Rules
  bare-handle   `let _ = <expr>.as_raw_handle` -- the #1759 no-op shape:
                acquire a raw handle, discard the value, usually under a
                comment claiming a lock exists. Fires anywhere the line
                is live for windows, INCLUDING cfg(windows) arms (the
                shipped bug was one); never inside cfg(unix)-only code,
                where the Windows handle APIs do not exist at all.
                Always on: --only cannot disable it.
  unix-api      std::os::unix, as_raw_fd, flock::/FlockOutcome, and the
                libc symbols that do NOT exist on Windows' libc (kill,
                SIG*, waitpid, fork, setsid, termios, pty, flock,
                getppid). libc::c_int and libc::_exit resolve on every
                target; flagging them was tested and is wrong: main.rs
                calls _exit un-gated and the windows build links it.
  unix-proc     Command::new("sh"|"bash"|"zsh"|"/bin/sh"|"login") where
                Command is bare or ::-qualified. A negative lookbehind
                over \w would have been WRONG (the colon of std::process::
                is itself a word boundary match); the guard is (?! \w | ::)
                BEFORE the name -- BotCommand:: trips nothing, tokio::
                process::Command:: trips the rule. A spawn is a spawn.
  unix-path     a "/tmp/..." string literal. Skipped under src/tests/:
                string fixtures pass on every OS, and test portability
                is the windows test slice's job, not shipped-code rules.

Suppression
  // windows-footgun: ok -- <reason>   on the offending line or the line
  above it. The reason is mandatory; a marker without one is itself a
  finding (bad-suppression), because an unexplained marker is how a
  guardrail quietly dies.

Exit codes: 0 clean, 1 findings, 2 bad usage. --self-test runs the
built-in negative fixtures: a linter that has silently learned to find
nothing is worse than no linter, so this ships its own proof of teeth.
"""
import argparse
import re
import sys
import tempfile
from pathlib import Path

UNIXY = re.compile(r"\bunix\b|\bmacos\b|\blinux\b|not\s*\(\s*windows\s*\)")
HAS_WIN = re.compile(r"\bwindows\b")
ATTR = re.compile(r"#\[!?[^\]]*\]")
MOD_DECL = re.compile(r"(?:pub(?:\([^)\n]*\))?\s+)?mod\s+(\w+)\s*;$")

RULES = {
    "bare-handle": (re.compile(r"let\s+_\s*=\s*[^;]*as_raw_handle"),
                    "raw handle acquired and dropped: this locks nothing"),
    "unix-api": (
        re.compile(
            r"std::os::unix|\bas_raw_fd\b"
            r"|\blibc::(?:kill|SIG[A-Z]+|waitpid|fork|setsid|tcgetattr|tcsetattr|flock|openpty|getppid)\b"
            r"|\bflock::|FlockOutcome"),
        "unix-only API is live for the windows build"),
    "unix-proc": (
        # (?:\w|:) lookbehind: bare Command::new only, but ::-qualified
        # paths (std::process::Command::new) MUST match; \w alone made the
        # colon before Command count as a word char and silenced the whole
        # rule for qualified spawns -- caught by --self-test, not by luck.
        re.compile(r"""(?<!\w)Command::new\(\s*"(?:/bin/)?(?:sh|bash|zsh|login)\s*"""),
        "unix shell spawn is live for the windows build"),
    "unix-path": (re.compile(r'"/tmp/'),
                  "POSIX /tmp literal is live for the windows build"),
}
OK_MARK = re.compile(r"windows-footgun:\s*ok\s*--")


def split_line(raw):
    """Return (code, same_line_suppressed); comment- and string-aware."""
    in_str = False
    prev = ""
    for i, ch in enumerate(raw):
        if ch == '"' and prev != "\\":
            in_str = not in_str
        elif not in_str and raw[i:i + 2] == "//":
            rest = raw[i:]
            if OK_MARK.search(rest):
                return raw[:i], True
            return raw[:i], False
        prev = ch
    return raw, False


def collect_declared_file_gates(root):
    """file -> cfg expression when its declaring `mod x;` is gated."""
    gates = {}
    for decl in sorted(root.rglob("*.rs")):
        try:
            lines = decl.read_text(encoding="utf-8").splitlines()
        except (UnicodeDecodeError, FileNotFoundError):
            continue
        attrs = []
        for ln in lines:
            st = ln.strip()
            if st.startswith("#["):
                if "cfg" in st:
                    attrs.append(st)
                continue
            m2 = MOD_DECL.match(st)
            if m2 and attrs:
                name = m2.group(1)
                for cand in (decl.parent / f"{name}.rs", decl.parent / name / "mod.rs"):
                    if cand.exists():
                        gates[cand] = " ".join(attrs)
            attrs = []
    return gates


def scan_file(path, rules, declared_gate=""):
    findings = []
    try:
        text = path.read_text(encoding="utf-8")
    except (UnicodeDecodeError, FileNotFoundError):
        return findings
    stack = []                  # (entry_depth, unixy_only)
    if declared_gate and UNIXY.search(declared_gate) and not HAS_WIN.search(declared_gate):
        stack.append((-1, True))
    depth = 0
    pending = ""                # accumulated cfg of the item being parsed
    pending_suppress = False    # own-line marker applies to the next line
    for lineno, raw in enumerate(text.splitlines(), 1):
        code, suppress = split_line(raw)
        stripped = code.strip()
        if not stripped and "windows-footgun:" in raw:
            reason = re.search(r"windows-footgun:\s*ok\s*--\s*(.*)", raw)
            if reason and reason.group(1).strip():
                pending_suppress = True
            else:
                findings.append((str(path), lineno, "bad-suppression",
                                 "ok marker needs a reason after --", raw.strip()[:110]))
            continue
        suppress = suppress or pending_suppress
        pending_suppress = False

        if "#![cfg(" in stripped:
            stack.append((-1, bool(UNIXY.search(stripped)) and not HAS_WIN.search(stripped)))
            continue

        while stack and stack[-1][0] >= depth:
            stack.pop()
        gated_here = any(u for _, u in stack)

        attr = ATTR.match(stripped)
        if attr and not stripped[len(attr.group(0)):].strip():
            if "cfg" in stripped:
                pending += stripped
            continue

        expr = pending
        item_unixy = bool(expr) and bool(UNIXY.search(expr)) and not HAS_WIN.search(expr)
        live_for_windows = not gated_here and not item_unixy

        opens, closes = code.count("{"), code.count("}")
        depth_before = depth

        if live_for_windows and not suppress:
            for name in rules:
                rx, msg = RULES[name]
                if rx.search(code):
                    findings.append((str(path), lineno, name, msg, stripped[:110]))

        if expr and (opens or ";" in stripped):
            pending = ""
        depth = max(0, depth + opens - closes)
        if opens and expr:
            stack.append((depth_before, item_unixy))
    return findings


SELF_BAD = r'''
use std::os::windows::io::AsRawHandle;
fn pretend(lock_file: &std::fs::File) {
    let _ = lock_file.as_raw_handle();
}
#[cfg(windows)]
fn sh() { let _ = std::process::Command::new("sh"); }
fn tmp() { let _ = std::fs::File::create("/tmp/x"); }
fn api() { unsafe { libc::kill(1, 2); } }
fn marker() {
    // windows-footgun: ok --
    let _ = 1;
}
'''

SELF_INLINE_GATES = r'''
#[cfg(unix)]
fn x() {
    unsafe { libc::kill(1, 2); }
}
mod m {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
    }
    // windows-footgun: ok -- reason present
    fn y() { let _ = "/tmp/marked"; }
    #[cfg(unix)]
    fn w() { let _ = h.as_raw_handle(); }
}
'''


def self_test():
    ok = True
    with tempfile.TemporaryDirectory() as td:
        d = Path(td)
        (d / "bad.rs").write_text(SELF_BAD)
        (d / "gates.rs").write_text(SELF_INLINE_GATES)
        names = [rule for _, _, rule, _, _ in scan_file(d / "bad.rs", set(RULES))]
        for want in ("bare-handle", "unix-proc", "unix-path", "unix-api", "bad-suppression"):
            if want not in names:
                print(f"self-test FAIL: rule {want} did not fire", file=sys.stderr)
                ok = False
        got = scan_file(d / "gates.rs", set(RULES))
        if got:
            print(f"self-test FAIL: inline gates leaked {got}", file=sys.stderr)
            ok = False
        (d / "decl.rs").write_text("#[cfg(unix)]\nmod gated;\n")
        (d / "gated.rs").write_text('fn z() { unsafe { libc::kill(1, 2); } }')
        gates = collect_declared_file_gates(d)
        if scan_file(d / "gated.rs", set(RULES), gates.get(d / "gated.rs", "")):
            print("self-test FAIL: declared-mod gate not honored", file=sys.stderr)
            ok = False
    print("self-test " + ("PASS" if ok else "FAIL"), file=sys.stderr)
    return 0 if ok else 1


def main():
    ap = argparse.ArgumentParser(description="Windows footgun gate")
    ap.add_argument("--root", default="src", type=Path)
    ap.add_argument("--only", default=",".join(sorted(RULES)))
    ap.add_argument("--quiet", action="store_true")
    ap.add_argument("--self-test", action="store_true",
                    help="run the built-in negative fixtures and exit")
    args = ap.parse_args()
    if args.self_test:
        return self_test()
    if not args.root.is_dir():
        print(f"error: {args.root} is not a directory", file=sys.stderr)
        return 2
    chosen = ({r.strip() for r in args.only.split(",")} | {"bare-handle"}) & set(RULES)
    unknown = chosen - set(RULES)
    if unknown:
        print(f"error: unknown rules {sorted(unknown)}", file=sys.stderr)
        return 2
    declared = collect_declared_file_gates(args.root)
    findings = []
    n = 0
    for f in sorted(args.root.rglob("*.rs")):
        n += 1
        in_tests = "/tests/" in f.as_posix() or f.parent.name == "tests"
        rules_here = chosen - ({"unix-path"} if in_tests else set())
        findings.extend(scan_file(f, rules_here, declared.get(f, "")))
    for path, lineno, rule, msg, snippet in findings:
        print(f"{path}:{lineno}: {rule}: {msg}")
        print(f"    {snippet}")
    if not args.quiet:
        print(f"{n} files scanned, {len(findings)} findings", file=sys.stderr)
    return 1 if findings else 0


if __name__ == "__main__":
    sys.exit(main())
