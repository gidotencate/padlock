#!/usr/bin/env python3
"""Layout conformance check: padlock's source-level layouts vs. real compilers.

Each <lang>/layout.<ext> is a compilable program whose main() prints one line
per struct: `Name <sizeof> <offset of last probed field>` (offset -1 = only the
size is comparable, e.g. non-extern Zig structs whose field order is
implementation-defined). The program is compiled and run for ground truth,
then `padlock explain` is run on the same file and the two are compared.

Known gaps are listed in xfail.txt as `lang:Name`. A listed case that now
passes (XPASS) fails the run so the entry gets removed.

Usage: run.py [--padlock PATH] [--lang go,c,cpp,rust,zig] [--update-xfail]
Tools: gcc, g++, rustc, go, zig (override with $ZIG). Missing tools are skipped.
"""
import argparse, os, re, shutil, subprocess, sys, tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))

def sh(cmd, **kw):
    return subprocess.run(cmd, capture_output=True, text=True, **kw)

def truth(lang, src, tmp):
    exe = os.path.join(tmp, "probe")
    if lang == "go":
        r = sh(["go", "run", src])
    elif lang == "c":
        r = sh(["gcc", "-std=gnu11", "-w", src, "-o", exe]); r = sh([exe]) if r.returncode == 0 else r
    elif lang == "cpp":
        r = sh(["g++", "-std=c++17", "-w", src, "-o", exe]); r = sh([exe]) if r.returncode == 0 else r
    elif lang == "rust":
        r = sh(["rustc", "-O", "--edition", "2021", src, "-o", exe]); r = sh([exe]) if r.returncode == 0 else r
    elif lang == "zig":
        r = sh([os.environ.get("ZIG", "zig"), "run", src, "--cache-dir", os.path.join(tmp, "zc")])
        r.stdout = r.stderr if r.returncode == 0 else r.stdout  # std.debug.print -> stderr
    if r.returncode != 0:
        sys.exit(f"{lang}: ground-truth probe failed:\n{r.stdout}{r.stderr}")
    out = {}
    for line in r.stdout.splitlines():
        p = line.split()
        if len(p) == 3:
            out[p[0]] = (int(p[1]), int(p[2]))
    return out

def padlock_layouts(padlock, src):
    out = sh([padlock, "explain", "--no-cache", src]).stdout
    cur, got = None, {}
    for l in out.splitlines():
        m = re.match(r"^(\S+)\s+\(.*:\d+\)$", l)
        if m:
            cur = m.group(1); got[cur] = {"rows": []}; continue
        m = re.match(r"^(\d+) bytes\s+align", l)
        if m and cur:
            got[cur]["size"] = int(m.group(1)); continue
        m = re.match(r"^│\s+(\d+)\s+│\s+(\d+)\s+│.*│\s+(.+?)\s*│$", l)
        if m and cur and "<padding>" not in l:
            got[cur]["rows"].append(int(m.group(1)))
    return got

def have(lang):
    need = {"go": "go", "c": "gcc", "cpp": "g++", "rust": "rustc", "zig": os.environ.get("ZIG", "zig")}[lang]
    return shutil.which(need) is not None

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--padlock", default=os.path.join(ROOT, "target", "release", "padlock"))
    ap.add_argument("--lang", default="go,c,cpp,rust,zig")
    ap.add_argument("--update-xfail", action="store_true", help="rewrite xfail.txt from current failures")
    a = ap.parse_args()
    xfile = os.path.join(HERE, "xfail.txt")
    xfail = set()
    if os.path.exists(xfile):
        for l in open(xfile):
            l = l.split("#")[0].strip()
            if l: xfail.add(l)
    ext = {"go": "go", "c": "c", "cpp": "cpp", "rust": "rs", "zig": "zig"}
    failures, newx, total_bad = [], [], 0
    for lang in a.lang.split(","):
        if not have(lang):
            print(f"[{lang}] SKIP (toolchain not found)"); continue
        src = os.path.join(HERE, lang, f"layout.{ext[lang]}")
        with tempfile.TemporaryDirectory() as tmp:
            t = truth(lang, src, tmp)
        got = padlock_layouts(a.padlock, src)
        bad = 0
        for name, (sz, off) in t.items():
            g = got.get(name)
            last = g["rows"][-1] if g and g["rows"] else None
            ok = bool(g) and g.get("size") == sz and (off < 0 or last == off)
            key = f"{lang}:{name}"
            if ok and key in xfail:
                print(f"[{lang}] XPASS {name} (remove from xfail.txt)"); failures.append(key + " XPASS")
            elif not ok:
                bad += 1; newx.append(key)
                detail = f"truth {sz}/{off} padlock {g.get('size') if g else 'missing'}/{last}"
                if key in xfail:
                    print(f"[{lang}] xfail {name}: {detail}")
                else:
                    print(f"[{lang}] FAIL  {name}: {detail}"); failures.append(key)
        total_bad += bad
        print(f"[{lang}] {len(t) - bad}/{len(t)} match the compiler")
    if a.update_xfail:
        with open(xfile, "w") as f:
            f.write("# Known layout gaps vs. the real compiler (see run.py). Remove entries as they are fixed.\n")
            for k in sorted(newx): f.write(k + "\n")
        print(f"wrote {xfile}"); return 0
    if failures:
        print("\nunexpected:", *failures, sep="\n  "); return 1
    return 0

if __name__ == "__main__":
    sys.exit(main())
