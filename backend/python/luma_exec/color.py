"""Light colors: linear Rec. 2020, each channel 0..1.

Every color in a score is light in linear Rec. 2020, the pattern engine's
working space (``backend/crates/patterns/src/color_space.rs``). A light's
brightness is its peak channel. Hex codes and CSS colors are sRGB, so convert
them before writing a triple:

    luma.track.from_srgb("#ff8000")      # -> [0.70, 0.27, 0.04], linear Rec. 2020
    luma.track.from_srgb([1, 0.5, 0])    # the same color as sRGB 0..1

A plain color value or a gradient stop may also be written as "#RRGGBB"; the
tool converts it. A tuple is not converted: color=(1, 0.5, 0) is already
linear Rec. 2020. Rec. 2020 holds colors sRGB cannot: [0, 1, 0] is a deeper
green than any hex code.

The matrix is derived as the Rust one is: from the BT.709 and BT.2020
primaries and D65, as ITU-R BT.2087 publishes it.
"""
from __future__ import annotations

import re

import numpy as np


def _rgb_to_xyz(primaries, white=(0.3127, 0.3290)):
    column = lambda x, y: np.array([x / y, 1.0, (1.0 - x - y) / y])
    matrix = np.stack([column(*xy) for xy in primaries], axis=1)
    scale = np.linalg.solve(matrix, column(*white))
    return matrix * scale


_SRGB_TO_XYZ = _rgb_to_xyz([(0.640, 0.330), (0.300, 0.600), (0.150, 0.060)])
_REC2020_TO_XYZ = _rgb_to_xyz([(0.708, 0.292), (0.170, 0.797), (0.131, 0.046)])
SRGB_TO_REC2020 = np.linalg.solve(_REC2020_TO_XYZ, _SRGB_TO_XYZ)
REC2020_TO_SRGB = np.linalg.inv(SRGB_TO_REC2020)


def _decode(encoded):
    encoded = np.asarray(encoded, dtype=float)
    return np.where(encoded <= 0.04045, encoded / 12.92, ((encoded + 0.055) / 1.055) ** 2.4)


def _encode(linear):
    linear = np.asarray(linear, dtype=float)
    return np.where(linear <= 0.0031308, 12.92 * linear,
                    1.055 * np.maximum(linear, 0.0031308) ** (1 / 2.4) - 0.055)


def from_srgb(color) -> list[float]:
    """An sRGB color, "#RRGGBB" or three channels 0..1, as linear Rec. 2020."""
    if isinstance(color, str):
        if not re.fullmatch(r"#[0-9a-fA-F]{6}", color):
            raise ValueError("a hex color is #RRGGBB")
        color = [int(color[index:index + 2], 16) / 255 for index in (1, 3, 5)]
    srgb = np.asarray(color, dtype=float)
    if srgb.shape != (3,) or not np.all((srgb >= 0) & (srgb <= 1)):
        raise ValueError("an sRGB color is #RRGGBB or three channels in 0..1")
    return [float(v) for v in np.clip(SRGB_TO_REC2020 @ _decode(srgb), 0.0, 1.0)]


def to_display(light):
    """Light (linear Rec. 2020, any shape [..., 3]) as sRGB 0..1 for a plot.

    A color sRGB cannot show loses the channels that fall below zero and,
    when too bright, its brightness: good enough to look at, unlike the
    engine's own mapping, which keeps hue and lightness.
    """
    linear = np.asarray(light, dtype=float) @ REC2020_TO_SRGB.T
    linear = np.maximum(linear, 0.0)
    peak = np.maximum(linear.max(axis=-1, keepdims=True), 1.0)
    return _encode(linear / peak)
