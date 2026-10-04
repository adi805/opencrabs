#!/usr/bin/env python3
r"""Fail CI when a top-level item name is declared twice in one Rust file.

Why this exists. A merge can put two identical declarations in the same
file without ever raising a conflict, because git only conflicts on the
same line. On 2026-10-03 the upstream sync did exactly that to
src/config/mod.rs: the fork declared `pub(crate) mod winlock;` at line 10,
upstream declared the same name at line 29, both survived the merge, and
rustc answered with E0428 "the name `winlock` is defined multiple times".

Both lines sit inside a `cfg(windows)` gate, which is the part that makes
this expensive: on the Linux and macOS targets a gated declaration is not
expanded at all, so clippy and rustfmt report a clean tree. Only a build
for the gated target sees the collision, and that build costs 21 to 51
minutes. This script catches the same class in a python pass.

Scope, chosen to stay cheap and to stay honest about false positives:

  * Only column-0 items are compared. Indented items live inside a
    `mod`, `impl` or `extern` block, where repeating a name is normal and
    legal, and merge damage of this shape always lands at column 0.
  * Two declarations of one name conflict when their accumulated cfg
    attributes are both absent, when one is absent (the ungated item is
    compiled on every target, so the gated one collides there), or when
    the two attribute strings are identical.
  * Two declarations with DIFFERENT cfg expressions are left alone. That
    is the legitimate platform-split shape (`cfg(windows)` plus
    `cfg(unix)`), which rustc accepts. This script does not evaluate cfg
    expressions, so it does not try to prove two different gates overlap.

Kinds compared: mod, fn, struct, enum, trait, union, const, static, type.
`use` and `impl` are not items with a name to collide, so they are out.

Usage: check-duplicate-decls.py [--root src] [--self-test] [--quiet]
Exit codes: 0 clean, 1 findings, 2 bad usage.
"""
import argparse
import re
import sys
import tempfile
from pathlib import Path

# One top-level item declaration, optionally public, optionally preceded by
# the usual modifiers. Name is captured for the collision key, kind for the
# message. Kept deliberately narrow: a wide regex here becomes a wide blast
# radius in CI.
DECL = re.compile(
    r"^(?:pub(?:\([A-Za-z0-9_:]+\))?\s+)?"
    r"(?:(?:pub(?:\([A-Za-z0-9_:]+\))|unsafe|extern|const|async)\s+)*"
    r"(mod|fn|struct|enum|trait|union|type|const|static)\s+"
    r"([A-Za-z_][A-Za-z0-9_]*)\b"
)
ATTR = re.compile(r"^#!?\[[^\]]*\]")
CFG_ONLY = re.compile(r"^#\[cfg\(")


def scan(path):
    """Return (findings, decl_count) for one file.

    findings is a list of (path_str, lineno, name, first_lineno, cfg) tuples.
    """
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except (UnicodeDecodeError, FileNotFoundError):
        return [], 0

    findings = []
    attrs = []          # contiguous column-0 attribute lines above an item
    seen = {}           # (name) -> list of (lineno, cfg_key)
    total = 0

    for lineno, raw in enumerate(lines, 1):
        if raw[:1] in ("", " ", "\t"):
            # Indented: inside a block, or a continuation. Either way it is
            # not a file-top-level item, and it ends any attribute run.
            if raw.strip():
                attrs = []
            continue
        stripped = raw.strip()
        if ATTR.match(stripped):
            if CFG_ONLY.match(stripped):
                attrs.append(stripped)
            else:
                attrs = []      # a non-cfg attribute ends the gate run
            continue
        m = DECL.match(stripped)
        if not m:
            attrs = []
            continue
        kind, name = m.group(1), m.group(2)
        if name == "_":
            # `const _: () = assert!(...)` is the anonymous-const idiom: `_`
            # declares no name, so repeating it in one file is legal and rustc
            # accepts it (verified with rustc, edition 2021). Counting it would
            # report a compile-time assertion pair as a collision.
            attrs = []
            continue
        total += 1
        cfg = " ".join(attrs)
        prior = seen.setdefault(name, [])
        for first_line, first_cfg in prior:
            if not cfg or not first_cfg or cfg == first_cfg:
                findings.append((str(path), lineno, name, first_line, cfg or first_cfg))
                break
        prior.append((lineno, cfg))
        attrs = []

    return findings, total


FIXTURES = [
    # (name, file contents, expected number of findings)
    ("identical cfg gates collide",
     '#[cfg(windows)]\npub(crate) mod winlock;\n#[cfg(windows)]\npub(crate) mod winlock;\n', 1),
    ("two ungated decls collide",
     'mod alpha;\nmod alpha;\n', 1),
    ("ungated collides with gated",
     'pub fn helper() {}\n#[cfg(windows)]\npub fn helper() {}\n', 1),
    ("different gates are a legal platform split",
     '#[cfg(windows)]\nmod gated;\n#[cfg(unix)]\nmod gated;\n', 0),
    ("indented items in separate impl blocks stay quiet",
     'impl A {\n    fn run() {}\n}\nimpl B {\n    fn run() {}\n}\n', 0),
    ("cfg_attr and doc attributes are not gates",
     '#[cfg(windows)]\n#[cfg_attr(feature = "x", allow(dead_code))]\nmod winlock;\n'
     '#[cfg(windows)]\nmod winlock;\n', 1),
    ("anonymous const assertions do not collide",
     'const _: () = assert!(1 > 0);\nconst _: () = assert!(2 > 0);\n', 0),
]


def self_test():
    ok = True
    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        for i, (label, body, want) in enumerate(FIXTURES):
            f = root / f"fixture_{i}.rs"
            f.write_text(body, encoding="utf-8")
            got, _ = scan(f)
            if len(got) != want:
                print(f"self-test FAIL: {label}: expected {want} finding(s), "
                      f"got {len(got)}", file=sys.stderr)
                ok = False
        # A file that must not be parsed as a tree of items.
        (root / "noise.rs").write_text(
            'use std::io::Read;\n// mod fake;\nlet x = "mod quoted;"\n', encoding="utf-8")
        got, total = scan(root / "noise.rs")
        if got or total:
            print(f"self-test FAIL: noise.rs gave {got} findings, {total} decls",
                  file=sys.stderr)
            ok = False
    print(f"duplicate-decl self-test {'PASS' if ok else 'FAIL'} "
          f"({len(FIXTURES) + 1} fixtures)", file=sys.stderr)
    return 0 if ok else 1


def main():
    ap = argparse.ArgumentParser(description="duplicate top-level declaration gate")
    ap.add_argument("--root", default="src", type=Path)
    ap.add_argument("--quiet", action="store_true")
    ap.add_argument("--self-test", action="store_true",
                    help="run the built-in fixtures and exit")
    args = ap.parse_args()

    if args.self_test:
        return self_test()
    if not args.root.is_dir():
        print(f"error: {args.root} is not a directory", file=sys.stderr)
        return 2

    findings = []
    files = 0
    decls = 0
    for f in sorted(args.root.rglob("*.rs")):
        files += 1
        got, total = scan(f)
        decls += total
        findings.extend(got)

    for path_str, lineno, name, first, cfg in findings:
        gate = f" (both under `{cfg}`)" if cfg else ""
        print(f"{path_str}:{lineno}: dup-decl: `{name}` declared twice"
              f" at line {first}{gate}; the gated form is invisible to the"
              f" targets CI builds by default")
    if not args.quiet:
        print(f"{files} files, {decls} top-level decls, {len(findings)} finding(s)",
              file=sys.stderr)
    return 1 if findings else 0


if __name__ == "__main__":
    sys.exit(main())
