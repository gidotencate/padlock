#!/usr/bin/env python3
"""Regenerate docs/images/social-preview.png and docs/images/demo-terminal.gif.

    python3 docs/images/generate.py [path/to/padlock]

The terminal demo runs the real binary on a small struct, so its output is
always what padlock prints today. Needs Pillow and DejaVu Sans fonts.
"""
import os, re, shutil, subprocess, sys, tempfile
from PIL import Image, ImageDraw, ImageFont

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, "..", ".."))
PADLOCK = sys.argv[1] if len(sys.argv) > 1 else os.path.join(ROOT, "target", "release", "padlock")
FONTS = "/usr/share/fonts/truetype/dejavu/"

BG = (22, 14, 42)          # icon background
PANEL = (34, 24, 62)
LAV = (216, 180, 254)
VIO = (167, 139, 250)
TEXT = (232, 226, 244)
DIM = (150, 140, 175)
RED = (248, 113, 113)
GREEN = (74, 222, 128)
YELLOW = (250, 204, 21)
BLUE = (96, 165, 250)

STRUCT = """#[repr(C)]
pub struct Connection {
    pub active: bool,
    pub id: u64,
    pub retries: u8,
    pub timeout_ms: u32,
    pub port: u16,
}
"""


def font(name, size):
    return ImageFont.truetype(FONTS + name, size)


# ── social preview ───────────────────────────────────────────────────────────
def social_preview():
    W, H = 1280, 640
    im = Image.new("RGB", (W, H), BG)
    d = ImageDraw.Draw(im)
    d.text((120, 52), "padlock", font=font("DejaVuSansMono-Bold.ttf", 104), fill=LAV)
    d.text((128, 176), "The lint pass for struct memory layout", font=font("DejaVuSans.ttf", 34), fill=TEXT)
    d.text((128, 224), "C  ·  C++  ·  Rust  ·  Go  ·  Zig", font=font("DejaVuSansMono-Bold.ttf", 30), fill=VIO)

    # byte strips of the demo struct: 32 B with padding, 16 B reordered
    cell, gap, x0 = 32, 2, 128
    def strip(y, parts, label, size_label, badge=None):
        d.text((x0, y - 44), label, font=font("DejaVuSans-Bold.ttf", 24), fill=DIM)
        d.text((x0 + 150, y - 44), size_label, font=font("DejaVuSansMono-Bold.ttf", 24), fill=TEXT)
        x = x0
        for n, color, name in parts:
            w = n * cell - gap
            d.rounded_rectangle((x, y, x + w, y + cell + 14), radius=5, fill=color)
            if name and w >= 60:
                d.text((x + w / 2, y + (cell + 14) / 2), name, font=font("DejaVuSansMono-Bold.ttf", 17), fill=(15, 10, 30), anchor="mm")
            x += n * cell
        if badge:
            d.text((x + 24, y + 4), badge, font=font("DejaVuSans-Bold.ttf", 34), fill=GREEN)
    pad = (120, 52, 70)
    before = [(1, BLUE, ""), (7, pad, "pad"), (8, GREEN, "id: u64"), (1, BLUE, ""), (3, pad, "pad"),
              (4, VIO, "timeout"), (2, (251, 191, 36), ""), (6, pad, "pad")]
    after = [(8, GREEN, "id: u64"), (4, VIO, "timeout"), (2, (251, 191, 36), ""), (1, BLUE, ""), (1, BLUE, "")]
    strip(380, before, "before", "32 B", None)
    strip(500, after, "after", "16 B", "-50%")
    d.text((x0, 584), "padding wasted: 16 B per instance   ->   0 B", font=font("DejaVuSans.ttf", 20), fill=DIM)
    im.save(os.path.join(HERE, "social-preview.png"), optimize=True)


# ── terminal demo ────────────────────────────────────────────────────────────
def run(cmd, cwd):
    p = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True)
    return p.stdout, p.stderr


def wrap(text, cols):
    out = []
    for line in text.splitlines():
        while len(line) > cols:
            cut = line.rfind(" ", 0, cols)
            cut = cut if cut > 0 else cols
            out.append(line[:cut])
            line = "  " + line[cut:].lstrip()
        out.append(line)
    return out


def colour(line):
    s = line.lstrip()
    if s.startswith("[HIGH]") or line.startswith("[✗]"):
        return RED
    if s.startswith("[MEDIUM]") or line.startswith("[~]"):
        return YELLOW
    if line.startswith("[✓]"):
        return GREEN
    if line.startswith("+") or line.startswith("+ "):
        return GREEN
    if line.startswith("-") and not line.startswith("-" * 3):
        return RED
    if line.startswith("padlock: warning"):
        return YELLOW
    return TEXT


def terminal_demo():
    tmp = tempfile.mkdtemp()
    try:
        path = os.path.join(tmp, "connection.rs")
        open(path, "w").write(STRUCT)
        COLS = 112
        steps = []  # (command, [(text, colour)])
        out, _ = run([PADLOCK, "analyze", "--no-cache", "connection.rs"], tmp)
        steps.append(("padlock analyze connection.rs", wrap(out.strip("\n"), COLS)))
        out, err = run([PADLOCK, "fix", "connection.rs"], tmp)
        steps.append(("padlock fix connection.rs", wrap((err.strip() + "\n" + out.strip()).strip("\n"), COLS)))
        out, _ = run([PADLOCK, "analyze", "--no-cache", "connection.rs"], tmp)
        steps.append(("padlock analyze connection.rs", wrap(out.strip("\n"), COLS)))
    finally:
        shutil.rmtree(tmp)

    f = font("DejaVuSansMono.ttf", 14)
    fb = font("DejaVuSansMono-Bold.ttf", 14)
    cw = f.getlength("M")
    lh = 20
    pad_x, top = 22, 44
    W = int(COLS * cw + pad_x * 2)
    H = 560
    scroll_lines = (H - top - 14) // lh

    def render(lines, cursor=True):
        im = Image.new("RGB", (W, H), BG)
        d = ImageDraw.Draw(im)
        d.rectangle((0, 0, W, 30), fill=PANEL)
        for i, c in enumerate(((255, 95, 86), (255, 189, 46), (39, 201, 63))):
            d.ellipse((14 + i * 22, 9, 26 + i * 22, 21), fill=c)
        d.text((W / 2, 15), "padlock", font=f, fill=DIM, anchor="mm")
        visible = lines[-scroll_lines:]
        for i, (txt, col, bold) in enumerate(visible):
            d.text((pad_x, top + i * lh), txt, font=fb if bold else f, fill=col)
        return im

    frames, durations = [], []
    def add(lines, ms):
        frames.append(render(lines))
        durations.append(ms)

    lines = []
    for cmd, output in steps:
        typed = ""
        base = list(lines)
        for ch in range(0, len(cmd) + 1, 2):
            typed = cmd[:ch]
            add(base + [("$ " + typed + "▌", LAV, True)], 55)
        lines = base + [("$ " + cmd, LAV, True)]
        add(lines, 350)
        for line in output:
            lines.append((line, colour(line), line.startswith("[")))
            add(lines, 90)
        lines.append(("", TEXT, False))
        add(lines, 1700)
    durations[-1] = 4000
    pal = frames[0].convert("P", palette=Image.ADAPTIVE, colors=64)
    qframes = [fr.quantize(palette=pal, dither=Image.Dither.NONE) for fr in frames]
    qframes[0].save(os.path.join(HERE, "demo-terminal.gif"), save_all=True, append_images=qframes[1:],
                    duration=durations, loop=0, optimize=True, disposal=1)


if __name__ == "__main__":
    social_preview()
    terminal_demo()
    for n in ("social-preview.png", "demo-terminal.gif"):
        print(n, os.path.getsize(os.path.join(HERE, n)) // 1024, "KB")
