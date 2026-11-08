#!/usr/bin/env python3
"""Compose the website's and README's screenshots from raw app captures.

Each image is one app window, with rounded corners and a soft shadow, over a
backdrop made from Luma's own stage render: the lit, hazed stage at the top
dissolving through a halftone screen into a pale fog of its own light.

    python3 scripts/screenshots/composite.py SHOTS_DIR [OUT_DIR] [--backdrop IMAGE]

SHOTS_DIR is what `scripts/screenshots/capture` wrote (it holds showcase/*.png);
OUT_DIR defaults to www/public/screenshots. The backdrop is IMAGE when given,
else the stage capture (showcase/stage.png). Needs numpy and Pillow.
"""

import sys
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFilter

REPO = Path(__file__).resolve().parents[2]

# name: (capture, canvas size, window width as a share of the canvas,
#        backdrop crop centre as a share of the stage shot's width)
IMAGES = {
    "hero": ("arrange", (2400, 1560), 0.80, 0.50),
    "arrange": ("arrange", (2000, 1320), 0.84, 0.40),
    "live": ("live", (2000, 1320), 0.84, 0.60),
    "songs": ("songs", (2000, 1320), 0.84, 0.30),
    "settings": ("settings", (2000, 1320), 0.84, 0.70),
    "rig": ("rig", (2000, 1320), 0.84, 0.50),
}


def smoothstep(a, b, x):
    t = np.clip((x - a) / (b - a), 0, 1)
    return t * t * (3 - 2 * t)


def bayer(n):
    """An n×n ordered-dither threshold matrix, values in [0, 1)."""
    m = np.array([[0]])
    while m.shape[0] < n:
        m = np.block([[4 * m, 4 * m + 2], [4 * m + 3, 4 * m + 1]])
    return (m + 0.5) / m.size


def cover(img, size, centre_x):
    """Scale `img` to cover `size`, cropping around `centre_x` (a share of its width)."""
    w, h = size
    scale = max(w / img.width, h / img.height)
    scaled = img.resize((round(img.width * scale), round(img.height * scale)), Image.LANCZOS)
    left = int(np.clip(scaled.width * centre_x - w / 2, 0, scaled.width - w))
    top = int((scaled.height - h) * 0.35)
    return scaled.crop((left, top, left + w, top + h))


def backdrop(stage, size, centre_x):
    w, h = size
    art = cover(stage, size, centre_x).convert("RGB")
    art = np.asarray(art.filter(ImageFilter.GaussianBlur(1.2))).astype(np.float32) / 255
    # Lift the stage a little, and soften its saturated colour toward its own
    # grey: the render is dark and loud, and the backdrop is art.
    grey = art @ np.array([0.3, 0.59, 0.11], np.float32)
    art = art * 0.8 + grey[..., None] * 0.2
    art = np.clip(art * 1.2 + 0.03, 0, 1) ** 0.92

    # The fog: the stage's own light, spread wide and washed toward white.
    spread = Image.fromarray((art * 255).astype(np.uint8)).filter(ImageFilter.GaussianBlur(w / 14))
    spread = np.asarray(spread).astype(np.float32) / 255
    mean = art.reshape(-1, 3).mean(0)
    tint = mean / max(mean.max(), 1e-3)
    fog = 0.78 + 0.16 * tint
    fog = fog * 0.8 + spread * 0.35
    fog = np.clip(fog, 0, 1)

    # A halftone screen of the art: a dot per cell, sized by the cell's light.
    cell = max(6, w // 300)
    yy, xx = np.mgrid[0:h, 0:w]
    cy, cx = (yy // cell) * cell + cell / 2, (xx // cell) * cell + cell / 2
    small = Image.fromarray((art * 255).astype(np.uint8)).resize((w // cell + 1, h // cell + 1), Image.BOX)
    cells = np.asarray(small).astype(np.float32) / 255
    cell_rgb = cells[yy // cell, xx // cell]
    luma = cell_rgb @ np.array([0.3, 0.59, 0.11], np.float32)
    radius = np.sqrt(np.clip(luma * 1.6, 0.04, 1.0)) * cell * 0.62
    dot = (np.hypot(yy - cy + 0.5, xx - cx + 0.5) < radius)[..., None]

    y = (yy / h)[..., None]
    screened = np.where(dot, np.clip(cell_rgb * 1.15, 0, 1), fog * 0.9 + art * 0.1)
    # Top: the stage, its screen showing more as it falls toward the fog.
    out = art * (1 - smoothstep(0.08, 0.55, y)) + screened * smoothstep(0.08, 0.55, y)
    # Then an ordered dissolve, in blocks of the screen's cell, into the fog.
    threshold = bayer(8)[(yy // cell) % 8, (xx // cell) % 8][..., None]
    fade = smoothstep(0.35, 0.92, y)
    out = np.where(threshold < fade, fog, out)
    # A soft finish over the dissolve, so the bottom is fog and not grit.
    out = out * (1 - smoothstep(0.75, 1.0, y) * 0.7) + fog * smoothstep(0.75, 1.0, y) * 0.7
    return Image.fromarray((np.clip(out, 0, 1) * 255).astype(np.uint8))


def rounded(size, radius):
    mask = Image.new("L", (size[0] * 2, size[1] * 2), 0)
    ImageDraw.Draw(mask).rounded_rectangle((0, 0, mask.width - 1, mask.height - 1), radius * 2, fill=255)
    return mask.resize(size, Image.LANCZOS)


def traffic_lights(shot):
    """The window controls macOS draws in the space the title bar keeps for
    them, which a headless capture has no window to draw."""
    shot = shot.convert("RGB")
    scale = shot.width / 1440  # the capture's logical width
    draw = ImageDraw.Draw(shot)
    for i, (fill, rim) in enumerate(
        (((255, 95, 87), (226, 70, 63)), ((254, 188, 46), (223, 158, 30)), ((40, 200, 64), (30, 166, 48)))
    ):
        cx, cy, r = (20 + 20 * i) * scale, 19 * scale, 6 * scale
        draw.ellipse((cx - r, cy - r, cx + r, cy + r), fill=fill, outline=rim, width=max(1, round(scale / 2)))
    return shot


def window(shot, width):
    height = round(shot.height * width / shot.width)
    win = traffic_lights(shot).resize((width, height), Image.LANCZOS)
    radius = round(width * 0.012)
    mask = rounded(win.size, radius)
    # A hairline of light around the glass edge.
    edge = Image.new("L", (width * 2, height * 2), 0)
    ImageDraw.Draw(edge).rounded_rectangle((1, 1, edge.width - 2, edge.height - 2), radius * 2, outline=255, width=3)
    edge = edge.resize(win.size, Image.LANCZOS)
    win = Image.composite(Image.new("RGB", win.size, (255, 255, 255)), win, edge.point(lambda v: v * 0.16))
    win.putalpha(mask)
    return win


def shadow(canvas_size, box, mask, blur, alpha, offset):
    layer = Image.new("L", canvas_size, 0)
    layer.paste(mask.point(lambda v: v * alpha), (box[0], box[1] + offset))
    return layer.filter(ImageFilter.GaussianBlur(blur))


def compose(stage, shot, size, share, centre_x):
    canvas = backdrop(stage, size, centre_x).convert("RGBA")
    win = window(shot, round(size[0] * share))
    x = (size[0] - win.width) // 2
    y = round((size[1] - win.height) * 0.62)
    mask = win.getchannel("A")
    for blur, alpha, offset in ((size[0] / 30, 0.45, size[0] // 60), (size[0] / 120, 0.35, size[0] // 300)):
        s = shadow(size, (x, y), mask, blur, alpha, offset)
        canvas = Image.composite(Image.new("RGBA", size, (8, 6, 14, 255)), canvas, s)
    canvas.alpha_composite(win, (x, y))
    return canvas.convert("RGB")


def main():
    args = sys.argv[1:]
    backdrop_file = None
    if "--backdrop" in args:
        at = args.index("--backdrop")
        backdrop_file = Path(args[at + 1])
        del args[at : at + 2]
    if not args:
        sys.exit(__doc__)
    shots = Path(args[0]) / "showcase"
    out = Path(args[1]) if len(args) > 1 else REPO / "www" / "public" / "screenshots"
    out.mkdir(parents=True, exist_ok=True)
    if backdrop_file is not None:
        stage = Image.open(backdrop_file).convert("RGB")
    else:
        stage = Image.open(shots / "stage.png")
        # Off with the stage's own chrome: the frame-rate card, the corner
        # buttons, the camera rail and the transport pill.
        w, h = stage.size
        stage = stage.crop((round(w * 0.04), round(h * 0.07), round(w * 0.94), round(h * 0.9)))
    for name, (capture, size, share, centre_x) in IMAGES.items():
        image = compose(stage, Image.open(shots / f"{capture}.png"), size, share, centre_x)
        path = out / f"{name}.webp"
        image.save(path, quality=86, method=6)
        print(f"{path.relative_to(REPO) if path.is_relative_to(REPO) else path}: {path.stat().st_size // 1024} KB")


if __name__ == "__main__":
    main()
