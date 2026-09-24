#!/usr/bin/env python3
"""Fetch the floor materials from Poly Haven and ambientCG and pack them.

Every set is CC0. This script is the only way the files next to it are made:
it downloads the exact 2K maps from each source's API, normalizes the albedo
and writes three JPEGs per set into `<id>/`:

- `albedo.jpg`: sRGB base colour, scaled so its mean linear luminance is the
  set's `albedo` in `floors.json`, and desaturated by its `desaturate`:
  ambientCG's procedural sets are more saturated than the scanned ones.
- `normal.jpg`: R and G the OpenGL (+Y up) tangent-space normal's X and Y
  as published (Z is rebuilt from them), B height, stretched to the full
  range.
- `surface.jpg`: R roughness, G ambient occlusion (white where the set has
  none), at 1K: both vary slowly, and at 2K they were half of every set's
  bytes. B is empty.

It also records each set's `mean_rgb` in `floors.json`: the written albedo's
mean linear colour, which the sky and the ambient light take as the ground's
colour. `fetch.py --means` recomputes it from the packed files alone.

The renderer reads these through `gpui/crates/render/src/floor.rs`. Run from
anywhere:

    python3 resources/meshes/floors/fetch.py [id ...]

The sets, their sources and their numbers are in `floors.json`, which the
renderer reads too. The script also writes `LICENSE.md`. Needs Pillow.
Downloads are cached in ~/.cache/luma-floor-sources.
"""

import io
import json
import re
import sys
import urllib.request
import zipfile
from pathlib import Path

from PIL import Image, ImageOps, ImageStat

HERE = Path(__file__).resolve().parent
CACHE = Path.home() / ".cache" / "luma-floor-sources"
SIZE = 2048
QUALITY = 85

# One entry per set: its source, the mean linear albedo its colour map is
# scaled to, how much it is desaturated, and the renderer's own numbers.
MANIFEST = json.loads((HERE / "floors.json").read_text())

PH_MAPS = {"albedo": "Diffuse", "normal": "nor_gl", "rough": "Rough", "height": "Displacement", "ao": "AO"}
ACG_MAPS = {"albedo": "Color", "normal": "NormalGL", "rough": "Roughness", "height": "Displacement", "ao": "AmbientOcclusion"}


def fetch(url: str) -> bytes:
    CACHE.mkdir(parents=True, exist_ok=True)
    path = CACHE / url.split("://", 1)[1].replace("/", "_").replace("?", "_").replace("=", "_")
    if not path.exists():
        print(f"  get {url}", file=sys.stderr)
        request = urllib.request.Request(url, headers={"User-Agent": "luma-floor-fetch"})
        with urllib.request.urlopen(request) as response:
            path.write_bytes(response.read())
    return path.read_bytes()


def polyhaven(asset: str) -> dict:
    files = json.loads(fetch(f"https://api.polyhaven.com/files/{asset}"))
    maps = {}
    for role, key in PH_MAPS.items():
        if key in files:
            maps[role] = Image.open(io.BytesIO(fetch(files[key]["2k"]["jpg"]["url"])))
    return maps


def ambientcg(asset: str) -> dict:
    archive = zipfile.ZipFile(io.BytesIO(fetch(f"https://ambientcg.com/get?file={asset}_2K-JPG.zip")))
    maps = {}
    for role, key in ACG_MAPS.items():
        name = f"{asset}_2K-JPG_{key}.jpg"
        if name in archive.namelist():
            maps[role] = Image.open(io.BytesIO(archive.read(name)))
    return maps


def to_linear(v: float) -> float:
    return v / 12.92 if v <= 0.04045 else ((v + 0.055) / 1.055) ** 2.4


def to_srgb(v: float) -> float:
    v = min(max(v, 0.0), 1.0)
    return v * 12.92 if v <= 0.0031308 else 1.055 * v ** (1 / 2.4) - 0.055


LINEAR = [to_linear(i / 255) for i in range(256)]


def mean_luminance(image: Image.Image) -> float:
    """Mean linear luminance, from each channel's histogram."""
    weights = (0.2126, 0.7152, 0.0722)
    total = 0.0
    for channel, weight in zip(image.split(), weights):
        histogram = channel.histogram()
        count = sum(histogram)
        total += weight * sum(n * LINEAR[i] for i, n in enumerate(histogram)) / count
    return total


def albedo(image: Image.Image, target: float, desaturate: float) -> Image.Image:
    image = image.convert("RGB").resize((SIZE, SIZE), Image.LANCZOS)
    if desaturate > 0:
        grey = ImageOps.grayscale(image).convert("RGB")
        image = Image.blend(image, grey, desaturate)
    scale = target / mean_luminance(image)
    table = [round(255 * to_srgb(LINEAR[i] * scale)) for i in range(256)]
    return image.point(table * 3)


def grey(image: Image.Image | None, fill: int) -> Image.Image:
    if image is None:
        return Image.new("L", (SIZE, SIZE), fill)
    return image.convert("L").resize((SIZE, SIZE), Image.LANCZOS)


def pack(floor: str) -> None:
    entry = MANIFEST[floor]
    source, asset = entry["source"], entry["asset"]
    target, desaturate = entry["albedo"], entry["desaturate"]
    print(f"{floor}: {source} {asset}", file=sys.stderr)
    maps = polyhaven(asset) if source == "Poly Haven" else ambientcg(asset)
    out = HERE / floor
    out.mkdir(exist_ok=True)
    base = albedo(maps["albedo"], target, desaturate)
    base.save(out / "albedo.jpg", quality=QUALITY)
    nx, ny, _ = maps["normal"].convert("RGB").resize((SIZE, SIZE), Image.LANCZOS).split()
    height = ImageOps.autocontrast(grey(maps.get("height"), 128), cutoff=0.5)
    Image.merge("RGB", (nx, ny, height)).save(out / "normal.jpg", quality=QUALITY, subsampling=0)
    half = (SIZE // 2, SIZE // 2)
    rough = grey(maps.get("rough"), 200).resize(half, Image.LANCZOS)
    ao = grey(maps.get("ao"), 255).resize(half, Image.LANCZOS)
    surface = Image.merge("RGB", (rough, ao, Image.new("L", half, 0)))
    surface.save(out / "surface.jpg", quality=QUALITY, subsampling=0)
    record_mean(floor)


def mean_rgb(image: Image.Image) -> list[float]:
    """Mean linear colour, from each channel's histogram."""
    means = []
    for channel in image.split():
        histogram = channel.histogram()
        means.append(sum(n * LINEAR[i] for i, n in enumerate(histogram)) / sum(histogram))
    return means


def record_mean(floor: str) -> None:
    """Write the packed albedo's mean linear colour into `floors.json`."""
    written = Image.open(HERE / floor / "albedo.jpg").convert("RGB")
    rgb = [round(v, 4) for v in mean_rgb(written)]
    print(f"  {floor}: mean albedo {mean_luminance(written):.3f} rgb {rgb}", file=sys.stderr)
    path = HERE / "floors.json"
    text = path.read_text()
    # The set's "albedo" line, which carries its colour numbers.
    start = text.index(f'\n  "{floor}": {{')
    line_start = text.index('    "albedo":', start)
    line_end = text.index("\n", line_start)
    line = re.sub(r', "mean_rgb": \[[^\]]*\]', "", text[line_start:line_end])
    line = line.rstrip(",") + f', "mean_rgb": [{rgb[0]}, {rgb[1]}, {rgb[2]}],'
    path.write_text(text[:line_start] + line + text[line_end:])


def license_notes() -> None:
    lines = [
        "# Floor materials",
        "",
        "Every set here is CC0 1.0 (public domain), from Poly Haven",
        "(https://polyhaven.com/license) or ambientCG (https://ambientcg.com/list?type=faq).",
        "`fetch.py` downloads and packs them; do not edit the images by hand.",
        "",
        "| Set | Source | Asset |",
        "|---|---|---|",
    ]
    for floor, entry in MANIFEST.items():
        lines.append(f"| `{floor}` | {entry['source']} | [{entry['asset']}]({entry['url']}) |")
    (HERE / "LICENSE.md").write_text("\n".join(lines) + "\n")


def main() -> None:
    args = sys.argv[1:]
    if args[:1] == ["--means"]:
        for floor in args[1:] or MANIFEST:
            record_mean(floor)
        return
    for floor in args or MANIFEST:
        pack(floor)
    license_notes()


if __name__ == "__main__":
    main()
