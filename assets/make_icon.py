"""Renders Archy's icon (blue "Clip A" on a dark rounded square) with no dependencies.

    python assets/make_icon.py [variant]  ->  assets/archy.ico, assets/archy.png (256 px)
    python assets/make_icon.py preview    ->  assets/icon-preview.png (all variants)

Shapes are drawn on a 24-unit grid with supersampled coverage, so every size
is anti-aliased.
"""
import math
import os
import struct
import sys
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
ACCENT = (77, 141, 255)  # Archy blue, #4D8DFF
BG = (17, 19, 22)
EDGE = (34, 42, 58)  # faint blue-tinted rim so the icon reads on dark taskbars
CORNER = 5.5


def seg_dist(px, py, a, b):
    (ax, ay), (bx, by) = a, b
    dx, dy = bx - ax, by - ay
    t = max(0.0, min(1.0, ((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy)))
    return math.hypot(px - (ax + t * dx), py - (ay + t * dy))


def in_polygon(x, y, pts):
    inside = False
    j = len(pts) - 1
    for i in range(len(pts)):
        (xi, yi), (xj, yj) = pts[i], pts[j]
        if (yi > y) != (yj > y) and x < (xj - xi) * (y - yi) / (yj - yi) + xi:
            inside = not inside
        j = i
    return inside


def rounded_square_dist(x, y, inset=0.0):
    lo, hi, r = inset, 24 - inset, CORNER - inset
    cx = min(max(x, lo + r), hi - r)
    cy = min(max(y, lo + r), hi - r)
    if lo <= x <= hi and lo <= y <= hi:
        return math.hypot(x - cx, y - cy) - r
    return 1e9


# Each variant: list of (kind, args). "stroke": polyline + width, "dot": centre + radius,
# "poly": filled polygon, "hole": polygon cut back to background.
# Tray icon while recording: the Clip A plus a small red "recording" dot.
REC_DOT = ((19.2, 4.8), 3.6)
REC_RED = (255, 77, 79)

VARIANTS = {
    # Bold chevron with a "recording" dot where the crossbar would be.
    "record": [
        ("stroke", [(5.0, 19.2), (12.0, 4.8), (19.0, 19.2)], 3.6),
        ("dot", (12.0, 15.2), 2.15),
    ],
    # Slanted, with a sharp rising crossbar: an A in motion.
    "speed": [
        ("stroke", [(4.6, 19.4), (13.4, 4.6), (19.4, 19.4)], 3.3),
        ("stroke", [(8.2, 15.6), (16.8, 12.0)], 2.6),
    ],
    # A rounded up-pointing play triangle with a triangular cut-out.
    "clip": [
        ("poly", [(12.0, 3.6), (20.6, 20.0), (3.4, 20.0)]),
        ("hole", [(12.0, 11.0), (15.3, 17.4), (8.7, 17.4)]),
    ],
}


def coverage(shapes, x, y):
    hit = False
    for kind, *args in shapes:
        if kind == "stroke":
            pts, w = args
            if any(seg_dist(x, y, pts[i], pts[i + 1]) <= w / 2 for i in range(len(pts) - 1)):
                hit = True
        elif kind == "dot":
            (cx, cy), r = args
            if math.hypot(x - cx, y - cy) <= r:
                hit = True
        elif kind == "poly":
            if in_polygon(x, y, args[0]):
                hit = True
        elif kind == "hole":
            if in_polygon(x, y, args[0]):
                hit = False
    return hit


def render(size, shapes, dot=False):
    ss = 4 if size >= 64 else 6
    px = []
    for j in range(size):
        row = []
        for i in range(size):
            acc = [0.0, 0.0, 0.0]
            cov = 0
            for sj in range(ss):
                for si in range(ss):
                    x = (i + (si + 0.5) / ss) * 24 / size
                    y = (j + (sj + 0.5) / ss) * 24 / size
                    d = rounded_square_dist(x, y)
                    if d > 0:
                        continue
                    cov += 1
                    color = ACCENT if coverage(shapes, x, y) else (EDGE if d > -0.6 else BG)
                    if dot:
                        (dx, dy), r = REC_DOT
                        dd = math.hypot(x - dx, y - dy)
                        if dd <= r:
                            color = REC_RED
                        elif dd <= r + 1.0:
                            color = BG  # dark ring separates the dot from the mark
                    for c in range(3):
                        acc[c] += color[c]
            if cov == 0:
                row.append((0, 0, 0, 0))
            else:
                row.append(tuple(round(v / cov) for v in acc) + (round(cov / (ss * ss) * 255),))
        px.append(row)
    return px


def png(px):
    h, w = len(px), len(px[0])
    raw = b"".join(b"\x00" + bytes(v for p in row for v in p) for row in px)

    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)

    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


def ico(images):
    # PNG-compressed ICO entries (supported since Windows Vista).
    header = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    entries, blobs = b"", b""
    for size, data in images:
        dim = 0 if size >= 256 else size
        entries += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
        blobs += data
    return header + entries + blobs


def preview():
    # Each variant at 128 px and 32 px (tray-ish size, scaled up 2x), side by side on grey.
    tiles = []
    for name, shapes in VARIANTS.items():
        big = render(128, shapes)
        small = render(32, shapes)
        tiles.append((big, small))
    pad, w = 24, 24 + len(tiles) * (128 + 64 + 48)
    h = 128 + 2 * pad
    canvas = [[(60, 62, 66, 255)] * w for _ in range(h)]
    x = pad
    for big, small in tiles:
        for j in range(128):
            for i in range(128):
                p = big[j][i]
                a = p[3] / 255
                base = canvas[pad + j][x + i]
                canvas[pad + j][x + i] = tuple(round(p[c] * a + base[c] * (1 - a)) for c in range(3)) + (255,)
        sx = x + 128 + 24
        for j in range(64):
            for i in range(64):
                p = small[j // 2][i // 2]
                a = p[3] / 255
                base = canvas[pad + 32 + j][sx + i]
                canvas[pad + 32 + j][sx + i] = tuple(round(p[c] * a + base[c] * (1 - a)) for c in range(3)) + (255,)
        x += 128 + 64 + 48
    with open(os.path.join(HERE, "icon-preview.png"), "wb") as f:
        f.write(png(canvas))
    print("wrote icon-preview.png (left to right: " + ", ".join(VARIANTS) + ")")


if __name__ == "__main__":
    arg = sys.argv[1] if len(sys.argv) > 1 else "clip"
    if arg == "preview":
        preview()
        sys.exit()
    shapes = VARIANTS[arg]
    sizes = [16, 20, 24, 32, 40, 48, 64, 128, 256]
    rec = [(s, png(render(s, shapes, dot=True))) for s in [16, 20, 24, 32, 40, 48, 64]]
    with open(os.path.join(HERE, "archy-rec.ico"), "wb") as f:
        f.write(ico(rec))
    images = [(s, png(render(s, shapes))) for s in sizes]
    with open(os.path.join(HERE, "archy.ico"), "wb") as f:
        f.write(ico(images))
    for s in (32, 128, 256):
        name = "archy.png" if s == 256 else f"archy-{s}.png"
        with open(os.path.join(HERE, name), "wb") as f:
            f.write(dict(images)[s])
    print(f"wrote archy.ico, archy.png ({arg})")
