#!/usr/bin/env python3
"""Layout conformance check: padlock's source-level layouts vs. real compilers.

Each <lang>/layout.<ext> is a compilable program whose main() prints one line
per struct: `Name <sizeof> <offset of last probed field>` (offset -1 = only the
size is comparable, e.g. non-extern Zig structs whose field order is
implementation-defined). The program is compiled and run for ground truth,
then `padlock explain` is run on the same file and the two are compared.

With `--backend dwarf` (or `both`, the default) the probe is also built with
debug info and padlock's binary analysis (DWARF reader) is checked against the
same truth; those cases are tagged `dwarf-<lang>`.

Known gaps are listed in xfail.txt as `lang:Name`. A listed case that now
passes (XPASS) fails the run so the entry gets removed.

Usage: run.py [--padlock PATH] [--lang go,c,cpp,rust,zig,c23] [--update-xfail]
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
    elif lang == "c23":
        # gcc < 14 lacks C23 `_BitInt`; zig's bundled clang has it.
        r = sh([os.environ.get("ZIG", "zig"), "cc", "-std=c23", "-w", src, "-o", exe])
        r = sh([exe]) if r.returncode == 0 else r
    elif lang == "zig":
        r = sh([os.environ.get("ZIG", "zig"), "run", src, "--cache-dir", os.path.join(tmp, "zc")])
        r.stdout = r.stderr if r.returncode == 0 else r.stdout  # std.debug.print -> stderr
    if r.returncode != 0:
        sys.exit(f"{lang}: ground-truth probe failed:\n{r.stdout}{r.stderr}")
    out = {}
    for line in r.stdout.splitlines():
        # `Name size offset [field]` — with a field name the offset is compared
        # against that field's row, otherwise against the last row. `Name` may
        # contain spaces (`Buf<char, 8>`), so parse from the right.
        m = re.match(r"^(.+?) (\d+) (-?\d+)(?: (\w+))?$", line.strip())
        if m:
            out[m.group(1)] = (int(m.group(2)), int(m.group(3)), m.group(4))
    return out

def debug_binary(lang, src, tmp):
    """Compile `src` with debug info for the DWARF backend; returns the binary path."""
    exe = os.path.join(tmp, "dbg")
    zig = os.environ.get("ZIG", "zig")
    keep = ["-fno-eliminate-unused-debug-types"]
    cmd = {
        "c": ["gcc", "-std=gnu11", "-w", "-g", "-O0", *keep, src, "-o", exe],
        # -femit-class-debug-always: describe dynamic classes even where their
        # key function (and so their vtable) is not defined.
        "cpp": ["g++", "-std=c++17", "-w", "-g", "-O0", *keep, "-femit-class-debug-always",
                src, "-o", exe],
        "c23": [zig, "cc", "-std=c23", "-w", "-g", "-O0", *keep, src, "-o", exe],
        "rust": ["rustc", "-g", "-C", "opt-level=0", "--edition", "2021", src, "-o", exe],
        "go": ["go", "build", "-gcflags=all=-N -l", "-o", exe, src],
        "zig": [zig, "build-exe", src, "-O", "Debug", f"-femit-bin={exe}",
                "--cache-dir", os.path.join(tmp, "zc"), "--global-cache-dir", os.path.join(tmp, "zgc")],
    }[lang]
    r = sh(cmd, cwd=tmp)
    if r.returncode != 0:
        sys.exit(f"{lang}: debug build failed:\n{r.stdout}{r.stderr}")
    return exe

def short_name(name):
    """`main.Arr` / `layout::Arr` / `layout.Arr` → `Arr` (DWARF names carry the module);
    spaces are dropped so `GPair[int8,int64]` matches `GPair[int8, int64]`."""
    return re.sub(r"^(?:[A-Za-z_][\w]*(?:::|\.))+(?=[A-Za-z_])", "", name).replace(" ", "")

def padlock_layouts(padlock, src):
    out = sh([padlock, "explain", "--no-cache", src]).stdout
    cur, got, prev = None, {}, ""
    for l in out.splitlines():
        m = re.match(r"^(\d+) bytes\s+align", l)
        if m:
            # The header line above: `Name  (file:line)` (source) or `Name` (binary).
            cur = re.sub(r"\s+\(.*:\d+\)$", "", prev.strip())
            got.setdefault(cur, {"rows": []})["size"] = int(m.group(1))
            continue
        if l.strip():
            prev = l
        m = re.match(r"^│\s+(\d+)\s+│\s+(\d+)\s+│.*│\s+(.+?)\s*│$", l)
        if m and cur and "<padding>" not in l:
            got[cur]["rows"].append((int(m.group(1)), m.group(3).split(":")[0].strip()))
    return got

def have(lang):
    need = {"go": "go", "c": "gcc", "cpp": "g++", "rust": "rustc",
            "zig": os.environ.get("ZIG", "zig"), "c23": os.environ.get("ZIG", "zig")}[lang]
    return shutil.which(need) is not None

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--padlock", default=os.path.join(ROOT, "target", "release", "padlock"))
    ap.add_argument("--lang", default="go,c,cpp,rust,zig,c23")
    ap.add_argument("--backend", default="both", choices=["source", "dwarf", "both"],
                    help="check padlock's source frontends, its DWARF reader on a debug build, or both")
    ap.add_argument("--update-xfail", action="store_true", help="rewrite xfail.txt from current failures")
    a = ap.parse_args()
    xfile = os.path.join(HERE, "xfail.txt")
    xfail = set()
    if os.path.exists(xfile):
        for l in open(xfile):
            l = l.split("#")[0].strip()
            if l: xfail.add(l)
    ext = {"go": "go", "c": "c", "cpp": "cpp", "rust": "rs", "zig": "zig", "c23": "c"}
    failures, newx, total_bad = [], [], 0
    backends = ["source", "dwarf"] if a.backend == "both" else [a.backend]
    runs = [(lang, b) for lang in a.lang.split(",") for b in backends]
    for lang, backend in runs:
        tag = lang if backend == "source" else f"dwarf-{lang}"
        if not have(lang):
            print(f"[{tag}] SKIP (toolchain not found)"); continue
        src = os.path.join(HERE, lang, f"layout.{ext[lang]}")
        with tempfile.TemporaryDirectory() as tmp:
            t = truth(lang, src, tmp)
            if backend == "source":
                got = padlock_layouts(a.padlock, src)
            else:
                got = {}
                layouts = padlock_layouts(a.padlock, debug_binary(lang, src, tmp))
                # The probe's own types (`main.Once`, `layout.In`) win over a
                # library type with the same short name (`sync.Once`).
                own = re.compile(r"^(main\.|layout(\.|::))")
                for name, v in sorted(layouts.items(), key=lambda kv: not own.match(kv[0])):
                    got.setdefault(short_name(name), v)
        bad = 0
        for name, (sz, off, field) in t.items():
            g = got.get(name if backend == "source" else short_name(name))
            rows = g["rows"] if g else []
            if field is not None:
                last = next((o for o, n in rows if n == field), None)
            else:
                last = rows[-1][0] if rows else None
            ok = bool(g) and g.get("size") == sz and (off < 0 or last == off)
            key = f"{tag}:{name}"
            if ok and key in xfail:
                print(f"[{tag}] XPASS {name} (remove from xfail.txt)"); failures.append(key + " XPASS")
            elif not ok:
                bad += 1; newx.append(key)
                detail = f"truth {sz}/{off} padlock {g.get('size') if g else 'missing'}/{last}"
                if key in xfail:
                    print(f"[{tag}] xfail {name}: {detail}")
                else:
                    print(f"[{tag}] FAIL  {name}: {detail}"); failures.append(key)
        total_bad += bad
        print(f"[{tag}] {len(t) - bad}/{len(t)} match the compiler")
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
