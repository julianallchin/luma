#!/usr/bin/env python3
"""Write the showcase library's song art and audio into harness/showcase/.

Everything is made here, from code: abstract cover art for each fictional
song, and one 96-second, 120 bpm track with an intro, a build, a drop, a
break and an outro, so its waveform reads like music. The outputs are
committed; run this again only to change them.

    python3 scripts/screenshots/make_assets.py

Needs numpy, Pillow and ffmpeg (for the MP3 encode).
"""

import math
import subprocess
import tempfile
import wave
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFilter

ROOT = Path(__file__).resolve().parents[2] / "harness" / "showcase"
SIZE = 512
RATE = 22050
BPM = 120


def grain(img, amount=10, seed=0):
    rng = np.random.default_rng(seed)
    a = np.asarray(img).astype(np.float32)
    a += rng.normal(0, amount, a.shape[:2])[..., None]
    return Image.fromarray(np.clip(a, 0, 255).astype(np.uint8))


def grid():
    y, x = np.mgrid[0:SIZE, 0:SIZE].astype(np.float32) / SIZE
    return x, y


def mix(c0, c1, t):
    t = np.clip(t, 0, 1)[..., None]
    return np.asarray(c0, np.float32) * (1 - t) + np.asarray(c1, np.float32) * t


def to_img(a):
    return Image.fromarray(np.clip(a, 0, 255).astype(np.uint8))


def night_bloom():
    x, y = grid()
    r = np.hypot(x - 0.5, y - 0.56)
    th = np.arctan2(y - 0.56, x - 0.5)
    petals = 0.5 + 0.5 * np.cos(th * 7)
    glow = np.exp(-((r / (0.18 + 0.12 * petals)) ** 2))
    a = mix((14, 6, 30), (230, 60, 160), glow)
    a = mix(a, (255, 214, 120), np.exp(-((r / 0.06) ** 2)))
    return grain(to_img(a), 9, 1)


def glass_harbour():
    x, y = grid()
    a = mix((10, 40, 70), (120, 210, 220), y)
    bands = (np.sin(y * 60) > 0.6) & (y > 0.62)
    a[bands] = a[bands] * 0.75 + np.array([255, 240, 220]) * 0.25
    sun = np.hypot(x - 0.5, y - 0.42) < 0.17
    a[sun & (y < 0.6)] = (250, 236, 210)
    return grain(to_img(a), 6, 2)


def undertow():
    img = Image.new("RGB", (SIZE, SIZE), (6, 16, 40))
    d = ImageDraw.Draw(img)
    for i in range(34):
        pts = []
        for s in range(0, SIZE + 8, 8):
            pts.append((s, 40 + i * 14 + 18 * math.sin(s / 50 + i * 0.35)))
        shade = int(60 + i * 5)
        d.line(pts, fill=(30, shade, min(255, shade + 90)), width=3)
    return grain(img.filter(ImageFilter.GaussianBlur(0.6)), 7, 3)


def low_orbit():
    x, y = grid()
    a = np.zeros((SIZE, SIZE, 3), np.float32) + (8, 8, 12)
    r = np.hypot(x - 0.5, y - 1.25)
    planet = r < 0.62
    a[planet] = mix((255, 120, 40), (90, 20, 10), (y[planet] - 0.6) * 2.5)
    rim = np.exp(-(((r - 0.62) / 0.02) ** 2))
    a = a + rim[..., None] * np.array([255, 200, 140])
    rng = np.random.default_rng(4)
    for _ in range(140):
        px, py = rng.integers(0, SIZE, 2)
        if py < SIZE * 0.55:
            a[py, px] = 200 + rng.integers(0, 55)
    return grain(to_img(a), 5, 4)


def paper_satellites():
    img = Image.new("RGB", (SIZE, SIZE), (236, 228, 212))
    d = ImageDraw.Draw(img)
    for i, (cx, cy, rr, col) in enumerate(
        [
            (190, 220, 130, (220, 70, 50)),
            (330, 300, 90, (30, 50, 90)),
            (300, 150, 40, (240, 180, 40)),
        ]
    ):
        d.ellipse([cx - rr, cy - rr, cx + rr, cy + rr], fill=col)
    for k in range(6):
        d.arc([60 - k * 8, 60 - k * 8, 452 + k * 8, 452 + k * 8], 200, 330, fill=(30, 30, 30), width=2)
    return grain(img, 8, 5)


def saltwater_static():
    img = Image.new("RGB", (SIZE, SIZE), (4, 30, 40))
    d = ImageDraw.Draw(img)
    step = 16
    for gy in range(0, SIZE, step):
        for gx in range(0, SIZE, step):
            t = 0.5 + 0.5 * math.sin(gx / 70 + math.cos(gy / 60) * 2)
            rr = 1 + t * 6.5
            c = (int(40 + 180 * t), int(200 + 50 * t), int(210 + 40 * t))
            d.ellipse([gx + 8 - rr, gy + 8 - rr, gx + 8 + rr, gy + 8 + rr], fill=c)
    return grain(img, 5, 6)


def afterglow_theory():
    x, y = grid()
    a = mix((40, 10, 70), (255, 120, 80), y * 1.4)
    a = mix(a, (255, 220, 150), np.clip((y - 0.55) * 4, 0, 1) * (y < 0.6))
    floor = y > 0.6
    gl = ((np.abs(((x - 0.5) / (y - 0.55 + 1e-3)) * 4) % 1) < 0.06) | (((1 / (y - 0.55 + 1e-3)) % 1.5) < 0.15)
    a[floor] = (20, 6, 40)
    a[floor & gl] = (255, 80, 200)
    return grain(to_img(a), 6, 7)


def neon_pilgrim():
    x, y = grid()
    s = (x + y) * 9
    stripe = (s % 1) < 0.18
    a = np.zeros((SIZE, SIZE, 3), np.float32) + (12, 10, 22)
    a[stripe] = mix((60, 240, 255), (255, 60, 220), x)[stripe]
    img = to_img(a).filter(ImageFilter.GaussianBlur(1.5))
    glow = img.filter(ImageFilter.GaussianBlur(12))
    return grain(Image.blend(img, glow, 0.4), 6, 8)


COVERS = {
    "night-bloom": night_bloom,
    "glass-harbour": glass_harbour,
    "undertow": undertow,
    "low-orbit": low_orbit,
    "paper-satellites": paper_satellites,
    "saltwater-static": saltwater_static,
    "afterglow-theory": afterglow_theory,
    "neon-pilgrim": neon_pilgrim,
}


def envelope(n, attack, decay):
    t = np.arange(n) / RATE
    return np.minimum(t / attack, 1) * np.exp(-t / decay)


def track():
    beat = 60 / BPM
    bars = 48
    total = int(bars * 4 * beat * RATE)
    out = np.zeros(total, np.float32)
    rng = np.random.default_rng(11)
    t_kick = np.arange(int(0.4 * RATE)) / RATE
    kick = np.sin(2 * np.pi * (45 * t_kick + 90 * (1 - np.exp(-t_kick * 30)) / 30)) * envelope(len(t_kick), 0.002, 0.18)
    hat = rng.normal(0, 1, int(0.06 * RATE)) * envelope(int(0.06 * RATE), 0.001, 0.015)
    hat = np.diff(hat, prepend=0)
    clap = rng.normal(0, 1, int(0.2 * RATE)) * envelope(int(0.2 * RATE), 0.003, 0.06)
    roots = [55.0, 55.0, 43.65, 49.0]

    def put(sample, at, gain):
        i = int(at * RATE)
        j = min(total, i + len(sample))
        out[i:j] += sample[: j - i] * gain

    for b in range(bars * 4):
        at = b * beat
        bar = b // 4
        intro, build, drop, brk = bar < 8, 8 <= bar < 16, 16 <= bar < 32, 32 <= bar < 40
        outro = bar >= 40
        if not (intro or brk):
            put(kick, at, 0.9 if drop else 0.7 if build else 0.6 * (1 - (bar - 40) / 10))
        if not brk:
            put(hat, at + beat / 2, 0.25 if drop else 0.15)
        if drop and b % 2 == 1:
            put(clap, at, 0.45)
        if build and bar >= 12:
            steps = 2 if bar < 14 else 4
            for k in range(steps):
                put(clap, at + k * beat / steps, 0.12 + 0.3 * (bar - 12) / 4)
        if drop or (outro and bar < 44):
            f = roots[bar % 4]
            n = int(beat * RATE)
            tt = np.arange(n) / RATE
            saw = 2 * ((tt * f) % 1) - 1
            put(saw * envelope(n, 0.01, 0.25) * 0.35, at + beat / 2, 1.0)
    # A pad under the intro, the break and the outro, swelling into the drop.
    tt = np.arange(total) / RATE
    pad = sum(np.sin(2 * np.pi * f * tt + k) for k, f in enumerate([220, 277.2, 329.6, 440]))
    bar_at = tt / (4 * beat)
    level = np.where(bar_at < 8, 0.06 + 0.04 * bar_at / 8, 0)
    level = np.where((bar_at >= 8) & (bar_at < 16), 0.1 + 0.1 * (bar_at - 8) / 8, level)
    level = np.where((bar_at >= 32) & (bar_at < 40), 0.18 * np.sin(np.pi * (bar_at - 32) / 8) + 0.04, level)
    level = np.where(bar_at >= 40, 0.05 * (48 - bar_at) / 8, level)
    out += pad * level * 0.5
    out /= np.max(np.abs(out)) * 1.05
    return out


def main():
    (ROOT / "art").mkdir(parents=True, exist_ok=True)
    for name, make in COVERS.items():
        make().convert("RGB").resize((384, 384), Image.LANCZOS).save(ROOT / "art" / f"{name}.jpg", quality=86, optimize=True)
    samples = (track() * 32767).astype(np.int16)
    with tempfile.TemporaryDirectory() as tmp:
        wav_path = Path(tmp) / "track.wav"
        with wave.open(str(wav_path), "wb") as w:
            w.setnchannels(1)
            w.setsampwidth(2)
            w.setframerate(RATE)
            w.writeframes(samples.tobytes())
        subprocess.run(
            ["ffmpeg", "-y", "-loglevel", "error", "-i", str(wav_path), "-c:a", "libmp3lame", "-b:a", "56k",
             str(ROOT / "night-bloom.mp3")],
            check=True,
        )


if __name__ == "__main__":
    main()
