#!/usr/bin/env python3
"""Draw the DMG window's background: dmg-background.png and its @2x.

Run it after changing the layout here or in dmg-settings.py; both PNGs are
checked in, so a release build needs no Pillow. The geometry below must match
dmg-settings.py: the window is WIDTH x HEIGHT points, and each icon sits at
the centre of its tile.
"""

from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter, ImageFont

WIDTH, HEIGHT = 660, 420
APP, APPLICATIONS = (180, 196), (480, 196)
TILE = (168, 188)
HERE = Path(__file__).resolve().parent
FONT = HERE.parents[2] / "harness" / "fonts" / "Inter-Regular.ttf"

BACKGROUND = (8, 8, 8)
BEAM = (136, 192, 208)  # ladder::accent_light, the icon's cyan
TEXT = (163, 163, 163)  # ladder::muted_foreground


def draw(scale: int) -> Image.Image:
    w, h = WIDTH * scale, HEIGHT * scale
    s = lambda v: round(v * scale)  # noqa: E731

    image = Image.new("RGB", (w, h), BACKGROUND)

    # A fan of faint beams from below the window, the icon's shape at stage
    # scale. Drawn sharp, then blurred into haze.
    beams = Image.new("L", (w, h), 0)
    pen = ImageDraw.Draw(beams)
    origin = (w * 0.5, h * 1.35)
    for x, spread, level in [
        (-0.05, 0.04, 70),
        (0.22, 0.03, 52),
        (0.5, 0.05, 80),
        (0.78, 0.03, 52),
        (1.05, 0.04, 70),
    ]:
        top = (w * x, -h * 0.1)
        dx = w * spread
        pen.polygon([origin, (top[0] - dx, top[1]), (top[0] + dx, top[1])], fill=level)
    beams = beams.filter(ImageFilter.GaussianBlur(s(18)))
    glow = Image.new("RGB", (w, h), BEAM)
    image = Image.composite(glow, image, beams)

    # A soft pool of light along the bottom edge, where the beams start.
    pool = Image.new("L", (w, h), 0)
    ImageDraw.Draw(pool).ellipse([w * 0.15, h * 0.88, w * 0.85, h * 1.25], fill=60)
    pool = pool.filter(ImageFilter.GaussianBlur(s(40)))
    image = Image.composite(glow, image, pool)

    # Glass tiles behind the two icons, as the app draws its cards.
    tiles = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    pen = ImageDraw.Draw(tiles)
    for cx, cy in (APP, APPLICATIONS):
        box = [s(cx - TILE[0] / 2), s(cy - TILE[1] / 2 + 10), s(cx + TILE[0] / 2), s(cy + TILE[1] / 2 + 10)]
        pen.rounded_rectangle(box, radius=s(22), fill=(255, 255, 255, 12), outline=(255, 255, 255, 30), width=max(1, s(1)))
    image = Image.alpha_composite(image.convert("RGBA"), tiles)

    # The arrow from one tile to the other, with a glow under it.
    left = APP[0] + TILE[0] / 2 + 22
    right = APPLICATIONS[0] - TILE[0] / 2 - 22
    y = APP[1]
    arrow = Image.new("L", (w, h), 0)
    pen = ImageDraw.Draw(arrow)
    pen.line([(s(left), s(y)), (s(right), s(y))], fill=255, width=s(2))
    pen.line([(s(right - 9), s(y - 8)), (s(right), s(y)), (s(right - 9), s(y + 8))], fill=255, width=s(2), joint="curve")
    halo = arrow.filter(ImageFilter.GaussianBlur(s(6))).point(lambda v: v * 0.8)
    image = Image.composite(Image.new("RGBA", (w, h), BEAM + (255,)), image, halo)
    image = Image.composite(Image.new("RGBA", (w, h), (230, 244, 248, 255)), image, arrow)

    # One line of instruction under the tiles.
    font = ImageFont.truetype(str(FONT), s(13))
    text = "Drag Luma into Applications"
    pen = ImageDraw.Draw(image)
    width = pen.textlength(text, font=font)
    pen.text(((w - width) / 2, s(342)), text, font=font, fill=TEXT)

    return image.convert("RGB")


def main() -> None:
    draw(1).save(HERE / "dmg-background.png", optimize=True)
    draw(2).save(HERE / "dmg-background@2x.png", optimize=True)


if __name__ == "__main__":
    main()
