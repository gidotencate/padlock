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

def truth(lang, src, tmp, cross=None):
    exe = os.path.join(tmp, "probe")
    if lang == "go":
        env = dict(os.environ, GOARCH=cross) if cross else None
        r = sh(["go", "run", src], env=env)
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
        r = sh([os.environ.get("ZIG", "zig"), "run", src, "--cache-dir", os.path.join(tmp, "zc"),
                *(["-target", cross] if cross else [])])
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

# (zig cc target, padlock --target name) pairs for `--cross`.
CROSS_TARGETS = [
    ("aarch64-linux-gnu", "aarch64"),
    ("riscv64-linux-gnu", "riscv64"),
    ("x86-linux-gnu", "x86"),
    ("thumb-freestanding-eabi -mcpu=cortex_m4", "cortex_m4"),
    ("thumb-freestanding-eabi -mcpu=cortex_m0", "cortex_m"),
    ("avr-freestanding -mmcu=atmega328p", "avr"),
]

def cross_truth(src, target, tmp):
    """Sizes/offsets of the C probe's `P(T, L)` structs as compiled for `target`.

    The probe cannot run there, so each `P(T, L)` becomes two file-scope arrays
    whose *symbol sizes* are `sizeof(struct T)` and `offsetof(struct T, L) + 1`,
    read back from the object's symbol table. Structs the target cannot compile
    (`__int128` on 32-bit, …) are dropped and retried.
    """
    zig = os.environ.get("ZIG", "zig")
    text = open(src).read()
    head = text[: text.index("#define P(T,L)")]
    head = head.replace("#include <stdio.h>", "").replace("#include <wchar.h>", "")
    calls = re.findall(r"P\((\w+),\s*(\w+)\)", text[text.index("int main"):])
    drop = set()
    for _ in range(12):
        body = [l for l in head.splitlines()
                if not any(re.match(rf"^\s*(typedef\s+)?struct\s+{d}\b", l) for d in drop)]
        probe = "\n".join(body) + "\n#include <stddef.h>\n"
        for t, l in calls:
            if t in drop:
                continue
            probe += (f"__attribute__((used)) static char __pl_sz_{t}[sizeof(struct {t})];\n"
                      f"__attribute__((used)) static char __pl_off_{t}__{l}[offsetof(struct {t}, {l}) + 1];\n")
        path = os.path.join(tmp, "cross.c")
        open(path, "w").write(probe)
        obj = os.path.join(tmp, "cross.o")
        r = sh([zig, "cc", *("-target " + target).split(), "-std=gnu11", "-w", "-c", path, "-o", obj])
        if r.returncode == 0:
            break
        bad = set(re.findall(r"struct (\w+)", r.stderr)) - drop
        bad |= {t for t, _ in calls for line in r.stderr.splitlines()
                if "error" in line and re.search(rf"\b{t}\b", line)}
        if not bad:
            sys.exit(f"cross probe for {target} failed:\n{r.stderr[-2000:]}")
        drop |= bad
    else:
        sys.exit(f"cross probe for {target} kept failing")
    syms = sh(["readelf", "-sW", obj]).stdout
    sizes, offs = {}, {}
    for line in syms.splitlines():
        parts = line.split()
        if len(parts) < 8:
            continue
        size, name = parts[2], parts[-1]
        if not re.match(r"^(0x[0-9a-fA-F]+|\d+)$", size):
            continue
        size = int(size, 0) if size.startswith("0x") else int(size)
        m = re.match(r"^(?:\w+\.)?__pl_sz_(\w+?)(?:\.\d+)?$", name)
        if m:
            sizes[m.group(1)] = size
        m = re.match(r"^(?:\w+\.)?__pl_off_(\w+?)__(\w+?)(?:\.\d+)?$", name)
        if m:
            offs[m.group(1)] = (size - 1, m.group(2))
    return {t: (sizes[t], offs[t][0], offs[t][1]) for t in sizes if t in offs}, path

def clang_layouts(src_text, triple, tmp, preprocess_target):
    """`{name: (size, {field: offset})}` from clang's own record-layout dump
    (`-fdump-record-layouts`) for `triple`; works for targets nothing here can run
    or read (MSVC / COFF). Headers come from `preprocess_target`'s libc. Structs
    the target rejects (e.g. a 40-bit `long` bit-field on LLP64) are dropped."""
    zig = os.environ.get("ZIG", "zig")
    lines = src_text.splitlines()
    for _ in range(12):
        raw = os.path.join(tmp, "raw.c")
        open(raw, "w").write("\n".join(lines) + "\n")
        pp = os.path.join(tmp, "pp.c")
        r = sh([zig, "cc", "-target", preprocess_target, "-E", "-P", raw, "-o", pp])
        if r.returncode != 0:
            sys.exit(f"preprocessing for {triple} failed:\n{r.stderr[-2000:]}")
        r = sh([zig, "clang", "-cc1", "-triple", triple, "-fdump-record-layouts",
                "-emit-llvm", "-o", os.devnull, "-w", pp])
        if r.returncode == 0:
            break
        pp_lines = open(pp).read().splitlines()
        bad = set()
        for ln in re.findall(r"pp\.c:(\d+):\d+: error", r.stderr):
            bad |= set(re.findall(r"struct (\w+)", pp_lines[int(ln) - 1]))
        if not bad:
            sys.exit(f"clang layout dump for {triple} failed:\n{r.stderr[-2000:]}")
        lines = [l for l in lines if not any(re.search(rf"\bstruct {b}\b", l) for b in bad)]
    out = {}
    cur = None
    for line in r.stdout.splitlines():
        m = re.match(r"^\s+0 \| (?:struct|union|class) (\S+)$", line)
        if m:
            cur = m.group(1); out[cur] = [None, {}]; continue
        if cur is None:
            continue
        m = re.search(r"\|\s+\[sizeof=(\d+)", line)
        if m:
            out[cur][0] = int(m.group(1)); cur = None; continue
        # Direct members are indented by exactly 3 spaces after `|`.
        m = re.match(r"^\s+(\d+)(?::\d+-\d+)? \|   (?! )(.+?) (\w+)$", line)
        if m:
            out[cur][1].setdefault(m.group(3), int(m.group(1)))
    return out

def short_name(name):
    """`main.Arr` / `layout::Arr` / `layout.Arr` → `Arr` (DWARF names carry the module);
    spaces are dropped so `GPair[int8,int64]` matches `GPair[int8, int64]`."""
    return re.sub(r"^(?:[A-Za-z_][\w]*(?:::|\.))+(?=[A-Za-z_])", "", name).replace(" ", "")

def padlock_layouts(padlock, src, target=None):
    out = sh([padlock, "explain", "--no-cache", *(["--target", target] if target else []), src]).stdout
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
    ap.add_argument("--cross", action="store_true",
                    help="also check C layouts for other targets (zig cc + padlock --target)")
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
    if a.cross and shutil.which(os.environ.get("ZIG", "zig")) and shutil.which("readelf"):
        src = os.path.join(HERE, "c", "layout.c")
        for target, arch in CROSS_TARGETS:
            tag = f"{arch}-c"
            with tempfile.TemporaryDirectory() as tmp:
                t, probe = cross_truth(src, target, tmp)
                # Analyze the same probe source padlock would see, for that target.
                out = sh([a.padlock, "explain", "--no-cache", "--target", arch, probe]).stdout
                got = {}
                cur, prev = None, ""
                for l in out.splitlines():
                    m = re.match(r"^(\d+) bytes\s+align", l)
                    if m:
                        cur = re.sub(r"\s+\(.*:\d+\)$", "", prev.strip())
                        got.setdefault(cur, {"rows": []})["size"] = int(m.group(1)); continue
                    if l.strip():
                        prev = l
                    m = re.match(r"^│\s+(\d+)\s+│\s+(\d+)\s+│.*│\s+(.+?)\s*│$", l)
                    if m and cur and "<padding>" not in l:
                        got[cur]["rows"].append((int(m.group(1)), m.group(3).split(":")[0].strip()))
            bad = 0
            for name, (sz, off, field) in t.items():
                g = got.get(name)
                rows = g["rows"] if g else []
                o = next((o for o, n in rows if n == field), None)
                ok = bool(g) and g.get("size") == sz and o == off
                key = f"{tag}:{name}"
                if ok and key in xfail:
                    print(f"[{tag}] XPASS {name} (remove from xfail.txt)"); failures.append(key + " XPASS")
                elif not ok:
                    bad += 1; newx.append(key)
                    detail = f"truth {sz}/{off} padlock {g.get('size') if g else 'missing'}/{o}"
                    if key in xfail:
                        print(f"[{tag}] xfail {name}: {detail}")
                    else:
                        print(f"[{tag}] FAIL  {name}: {detail}"); failures.append(key)
            print(f"[{tag}] {len(t) - bad}/{len(t)} match the compiler")
        # Windows x64 (MSVC ABI), from clang's record-layout dump.
        text = open(src).read()
        head = text[: text.index("#define P(T,L)")]
        head = head.replace("#include <stdio.h>", "")
        calls = re.findall(r"P\((\w+),\s*(\w+)\)", text[text.index("int main"):])
        probe = head + "".join(f"struct {t} __pl_{t};\n" for t, _ in calls)
        tag = "msvc-c"
        with tempfile.TemporaryDirectory() as tmp:
            dump = clang_layouts(probe, "x86_64-pc-windows-msvc", tmp, "x86_64-windows-gnu")
            ppath = os.path.join(tmp, "probe.c")
            open(ppath, "w").write(probe)
            got = padlock_layouts(a.padlock, ppath, "x86_64-msvc")
        bad = 0
        for t, l in calls:
            if t not in dump or dump[t][0] is None or l not in dump[t][1]:
                continue
            sz, off = dump[t][0], dump[t][1][l]
            g = got.get(t)
            rows = g["rows"] if g else []
            o = next((o for o, n in rows if n == l), None)
            ok = bool(g) and g.get("size") == sz and o == off
            key = f"{tag}:{t}"
            if ok and key in xfail:
                print(f"[{tag}] XPASS {t} (remove from xfail.txt)"); failures.append(key + " XPASS")
            elif not ok:
                bad += 1; newx.append(key)
                detail = f"truth {sz}/{off} padlock {g.get('size') if g else 'missing'}/{o}"
                if key in xfail:
                    print(f"[{tag}] xfail {t}: {detail}")
                else:
                    print(f"[{tag}] FAIL  {t}: {detail}"); failures.append(key)
        n = sum(1 for t, l in calls if t in dump and dump[t][0] is not None and l in dump[t][1])
        print(f"[{tag}] {n - bad}/{n} match the compiler")
        # Go and Zig probes for 32-bit x86 run natively on an x86-64 host.
        for lang, cross, arch in [("go", "386", "x86"), ("zig", "x86-linux", "x86")]:
            if not have(lang):
                continue
            tag = f"{arch}-{lang}"
            src = os.path.join(HERE, lang, f"layout.{ext[lang]}")
            with tempfile.TemporaryDirectory() as tmp:
                t = truth(lang, src, tmp, cross)
            got = padlock_layouts(a.padlock, src, arch)
            bad = 0
            for name, (sz, off, field) in t.items():
                g = got.get(name)
                rows = g["rows"] if g else []
                o = next((o for o, n in rows if n == field), None) if field else (rows[-1][0] if rows else None)
                ok = bool(g) and g.get("size") == sz and (off < 0 or o == off)
                key = f"{tag}:{name}"
                if ok and key in xfail:
                    print(f"[{tag}] XPASS {name} (remove from xfail.txt)"); failures.append(key + " XPASS")
                elif not ok:
                    bad += 1; newx.append(key)
                    detail = f"truth {sz}/{off} padlock {g.get('size') if g else 'missing'}/{o}"
                    if key in xfail:
                        print(f"[{tag}] xfail {name}: {detail}")
                    else:
                        print(f"[{tag}] FAIL  {name}: {detail}"); failures.append(key)
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
