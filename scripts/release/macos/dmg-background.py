#!/usr/bin/env python3
"""Draw the DMG window's background: dmg-background.png and its @2x.

Run it after changing the layout here or in dmg-settings.py; both PNGs are
checked in, so a release build needs no Pillow. The geometry below must match
dmg-settings.py: the window is WIDTH x HEIGHT points, and the arrow runs
between the two icon centres.
"""

from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

WIDTH, HEIGHT = 660, 420
APP, APPLICATIONS = (180, 196), (480, 196)
ICON = 128
HERE = Path(__file__).resolve().parent
FONT = HERE.parents[2] / "harness" / "fonts" / "Inter-Regular.ttf"

BACKGROUND = (14, 14, 15)
ARROW = (90, 90, 92)
TEXT = (120, 120, 124)


def draw(scale: int) -> Image.Image:
    s = lambda v: round(v * scale)  # noqa: E731
    image = Image.new("RGB", (s(WIDTH), s(HEIGHT)), BACKGROUND)
    pen = ImageDraw.Draw(image)

    # A thin arrow from the app to the folder, clear of both icons.
    left = APP[0] + ICON / 2 + 24
    right = APPLICATIONS[0] - ICON / 2 - 24
    y = APP[1]
    width = max(1, s(1.5))
    pen.line([(s(left), s(y)), (s(right), s(y))], fill=ARROW, width=width)
    pen.line(
        [(s(right - 7), s(y - 6)), (s(right), s(y)), (s(right - 7), s(y + 6))],
        fill=ARROW,
        width=width,
        joint="curve",
    )

    font = ImageFont.truetype(str(FONT), s(12))
    text = "Drag Luma into Applications"
    x = (s(WIDTH) - pen.textlength(text, font=font)) / 2
    pen.text((x, s(342)), text, font=font, fill=TEXT)
    return image


def main() -> None:
    draw(1).save(HERE / "dmg-background.png", optimize=True)
    draw(2).save(HERE / "dmg-background@2x.png", optimize=True)


if __name__ == "__main__":
    main()
