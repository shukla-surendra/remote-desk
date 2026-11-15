"""Generates AppIcon.icns (stdlib only). Run: python3 packaging/macos/make_icon.py"""
import os, struct, subprocess, sys, tempfile, zlib

N = 1024
HERE = os.path.dirname(os.path.abspath(__file__))

def inside_rrect(x, y, x0, y0, x1, y1, r):
    if not (x0 <= x < x1 and y0 <= y < y1):
        return False
    cx = min(max(x, x0 + r), x1 - r)
    cy = min(max(y, y0 + r), y1 - r)
    return (x - cx) ** 2 + (y - cy) ** 2 <= r * r

# Classic mouse pointer: tip at top-left, tail at bottom-right.
CURSOR = [(462, 360), (462, 560), (510, 515), (545, 585), (575, 571), (541, 502), (605, 498)]

def in_polygon(x, y, pts):
    inside = False
    j = len(pts) - 1
    for i, (xi, yi) in enumerate(pts):
        xj, yj = pts[j]
        if (yi > y) != (yj > y) and x < (xj - xi) * (y - yi) / (yj - yi) + xi:
            inside = not inside
        j = i
    return inside

def pixel(x, y):
    # macOS icon grid: ~824px rounded square centred in 1024.
    if not inside_rrect(x, y, 100, 100, 924, 924, 185):
        return (0, 0, 0, 0)
    t = (y - 100) / 824  # vertical gradient
    bg = (int(232 - 40 * t), int(76 - 30 * t), int(61 - 20 * t), 255)
    white = (255, 255, 255, 255)
    # Monitor: bezel ring, stand, base.
    if inside_rrect(x, y, 250, 270, 774, 640, 40) and not inside_rrect(x, y, 295, 315, 729, 595, 12):
        return white
    if 470 <= x < 554 and 640 <= y < 720:
        return white
    if inside_rrect(x, y, 380, 712, 644, 760, 24):
        return white
    # Cursor arrow inside the screen.
    if in_polygon(x, y, CURSOR):
        return white
    return bg

def write_png(path):
    raw = bytearray()
    for y in range(N):
        raw.append(0)
        for x in range(N):
            raw.extend(pixel(x, y))
    def chunk(t, d):
        c = struct.pack(">I", len(d)) + t + d
        return c + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n")
        f.write(chunk(b"IHDR", struct.pack(">IIBBBBB", N, N, 8, 6, 0, 0, 0)))
        f.write(chunk(b"IDAT", zlib.compress(bytes(raw), 9)))
        f.write(chunk(b"IEND", b""))

def main():
    with tempfile.TemporaryDirectory() as tmp:
        src = os.path.join(tmp, "icon_1024.png")
        write_png(src)
        iconset = os.path.join(tmp, "AppIcon.iconset")
        os.mkdir(iconset)
        for size in (16, 32, 128, 256, 512):
            for scale in (1, 2):
                px = size * scale
                name = f"icon_{size}x{size}{'@2x' if scale == 2 else ''}.png"
                subprocess.run(["sips", "-z", str(px), str(px), src, "--out", os.path.join(iconset, name)],
                               check=True, capture_output=True)
        out = os.path.join(HERE, "AppIcon.icns")
        subprocess.run(["iconutil", "-c", "icns", iconset, "-o", out], check=True)
        print("wrote", out)

if __name__ == "__main__":
    sys.exit(main())
