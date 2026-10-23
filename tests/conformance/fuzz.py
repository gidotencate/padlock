#!/usr/bin/env python3
"""Differential fuzzing of padlock's C/C++ layouts against the real compiler.

Generates random structs (scalars, pointers, arrays with constant-expression
lengths, bit-fields, nested and anonymous aggregates, unions, enums, packed /
aligned attributes, `#pragma pack`, C++ inheritance), compiles a probe with
gcc/g++ to print `sizeof` and the offset of every named field, and diffs that
against `padlock explain`. Any mismatch is printed with its seed so it can be
reproduced and turned into a conformance probe.

Usage: fuzz.py [--seed N] [--rounds R] [--structs S] [--lang c|cpp] [--padlock PATH]
"""
import argparse, os, random, re, subprocess, sys, tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))

SCALARS = ["char", "signed char", "unsigned char", "short", "unsigned short", "int",
           "unsigned", "long", "unsigned long", "long long", "float", "double",
           "long double", "_Bool", "int8_t", "uint16_t", "int32_t", "uint64_t",
           "size_t", "intptr_t", "wchar_t", "void *", "char *", "const int *"]
BITFIELD_TYPES = ["unsigned", "int", "unsigned char", "unsigned short", "unsigned long long"]


class Gen:
    def __init__(self, rng, cpp):
        self.rng, self.cpp = rng, cpp
        self.n = 0
        self.defs = []          # emitted declarations, in order
        self.records = []       # (name, [field names]) of probe-able records
        self.consts = {}

    def name(self, p):
        self.n += 1
        return f"{p}{self.n}"

    def array_len(self):
        r = self.rng.random()
        if r < 0.5:
            return str(self.rng.randint(1, 9))
        c = self.name("K")
        v = self.rng.randint(1, 5)
        self.defs.append(f"#define {c} {v}")
        return self.rng.choice([c, f"{c} * 2 + 1", f"sizeof(short) * {c}"])

    def scalar(self):
        t = self.rng.choice(SCALARS)
        return "bool" if self.cpp and t == "_Bool" else t

    def field_type(self, depth):
        r = self.rng.random()
        if r < 0.55 or depth > 2:
            return self.scalar(), ""
        if r < 0.7:
            return self.rng.choice(SCALARS[:12]), f"[{self.array_len()}]"
        if r < 0.8 and self.records:
            other, _, kind = self.rng.choice(self.records)
            return ("" if self.cpp else f"{kind} ") + other, ""
        if r < 0.88:
            e = self.name("E")
            self.defs.append(f"enum {e} {{ {e}_A, {e}_B = {self.rng.choice([3, 300, 70000])} }};")
            return f"enum {e}", ""
        # inline nested aggregate
        kind = self.rng.choice(["struct", "union"])
        inner = "; ".join(f"{t} m{i}{a}" for i, (t, a) in
                          enumerate(self.field_type(depth + 1) for _ in range(self.rng.randint(1, 3))))
        return f"{kind} {{ {inner}; }}", ""

    def record(self):
        name = self.name("S")
        is_union = self.rng.random() < 0.12
        lines, fields = [], []
        nfields = self.rng.randint(1, 8)
        i = 0
        while i < nfields:
            r = self.rng.random()
            if r < 0.15:
                # a run of bit-fields
                for _ in range(self.rng.randint(1, 4)):
                    t = self.rng.choice(BITFIELD_TYPES)
                    maxw = {"unsigned": 32, "int": 31, "unsigned char": 8,
                            "unsigned short": 16, "unsigned long long": 64}[t]
                    w = self.rng.randint(0 if fields else 1, maxw)
                    if w == 0:
                        lines.append(f"    {t} : 0;")
                    else:
                        lines.append(f"    {t} b{i} : {w};")
                    i += 1
                continue
            t, arr = self.field_type(0)
            # Unique per record, so a derived class never shadows a base member.
            fname = f"{name.lower()}_f{i}" if self.cpp else f"f{i}"
            attr = ""
            if self.rng.random() < 0.05:
                attr = f" __attribute__((aligned({self.rng.choice([2, 4, 8, 16, 32])})))"
            lines.append(f"    {t} {fname}{arr}{attr};")
            fields.append(fname)
            i += 1
        tail = ""
        if not is_union and self.rng.random() < 0.08:
            tail = " __attribute__((packed))"
        elif self.rng.random() < 0.05:
            tail = f" __attribute__((aligned({self.rng.choice([8, 16, 64])})))"
        pragma = (not is_union) and self.rng.random() < 0.06
        kw = "union" if is_union else "struct"
        base = ""
        structs = [r for r in self.records if r[2] == "struct"]
        if self.cpp and not is_union and structs and self.rng.random() < 0.15:
            base = " : " + self.rng.choice(structs)[0]
        if pragma:
            self.defs.append(f"#pragma pack(push, {self.rng.choice([1, 2, 4])})")
        self.defs.append(f"{kw} {name}{base} {{\n" + "\n".join(lines) + f"\n}}{tail};")
        if pragma:
            self.defs.append("#pragma pack(pop)")
        if fields:
            self.records.append((name, fields, kw))


def probe_source(gen, cpp):
    head = ["#include <stdint.h>", "#include <stddef.h>", "#include <stdio.h>"]
    if not cpp:
        head.insert(0, "#include <stdbool.h>")
        head.append("#include <wchar.h>")
    body = "\n".join(gen.defs)
    lines = ["int main(void) {"]
    for name, fields, kind in gen.records:
        t = name if cpp else f"{kind} {name}"
        for f in fields:
            lines.append(f'  printf("{name} %zu {f} %zu\\n", sizeof({t}), offsetof({t}, {f}));')
    lines.append("  return 0;\n}")
    return "\n".join(head) + "\n" + body + "\n" + "\n".join(lines) + "\n"


def padlock_rows(padlock, src):
    out = subprocess.run([padlock, "explain", "--no-cache", src], capture_output=True, text=True).stdout
    got, cur, prev = {}, None, ""
    for l in out.splitlines():
        m = re.match(r"^(\d+) bytes\s+align", l)
        if m:
            cur = re.sub(r"\s{2}\([^()]*\)$", "", prev.strip())
            got[cur] = {"size": int(m.group(1)), "fields": {}}
            continue
        if l.strip():
            prev = l
        m = re.match(r"^│\s+(\d+)\s+│\s+(\d+)\s+│.*│\s+(.+?)\s*│$", l)
        if m and cur:
            got[cur]["fields"].setdefault(m.group(3).split(":")[0].strip(), int(m.group(1)))
    return got


def run_round(seed, args):
    rng = random.Random(seed)
    cpp = args.lang == "cpp"
    gen = Gen(rng, cpp)
    for _ in range(args.structs):
        gen.record()
    src_text = probe_source(gen, cpp)
    with tempfile.TemporaryDirectory() as tmp:
        src = os.path.join(tmp, "fuzz." + ("cpp" if cpp else "c"))
        open(src, "w").write(src_text)
        exe = os.path.join(tmp, "fuzz")
        cc = ["g++", "-std=gnu++17"] if cpp else ["gcc", "-std=gnu11"]
        r = subprocess.run(cc + ["-w", src, "-o", exe], capture_output=True, text=True)
        if r.returncode != 0:
            return None  # generator produced something the compiler rejects
        truth = subprocess.run([exe], capture_output=True, text=True).stdout
        got = padlock_rows(args.padlock, src)
        bad = []
        for line in truth.splitlines():
            name, size, field, off = line.split()
            g = got.get(name)
            if not g or g["size"] != int(size) or g["fields"].get(field) != int(off):
                bad.append((name, int(size), field, int(off),
                            g["size"] if g else None, g["fields"].get(field) if g else None))
        if bad and args.keep:
            keep = os.path.join(args.keep, f"fuzz-{seed}." + ("cpp" if cpp else "c"))
            open(keep, "w").write(src_text)
        return bad


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--rounds", type=int, default=20)
    ap.add_argument("--structs", type=int, default=25)
    ap.add_argument("--lang", choices=["c", "cpp"], default="c")
    ap.add_argument("--padlock", default=os.path.join(ROOT, "target", "release", "padlock"))
    ap.add_argument("--keep", help="directory to save failing sources in")
    a = ap.parse_args()
    total_bad, rejected = 0, 0
    for seed in range(a.seed, a.seed + a.rounds):
        bad = run_round(seed, a)
        if bad is None:
            rejected += 1
            continue
        for name, sz, field, off, psz, poff in bad[:5]:
            print(f"seed {seed}: {name}.{field}: gcc {sz}B/@{off}  padlock {psz}B/@{poff}")
        total_bad += len(bad)
    print(f"{a.rounds - rejected} rounds, {total_bad} mismatching fields"
          + (f" ({rejected} rounds rejected by the compiler)" if rejected else ""))
    return 1 if total_bad else 0


if __name__ == "__main__":
    sys.exit(main())
