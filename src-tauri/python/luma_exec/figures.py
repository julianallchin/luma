"""Matplotlib figure capture (design §14.7).

After every cell, every open pyplot figure is saved as a PNG into
`<workspace>/outputs/fig-<uuid>.png`, closed, and reported to the host as
`{"artifact_rel", "width", "height"}`. Because figures are closed after each
cell, "open at the end of the cell" is exactly "created or touched by the cell".

Caps (contract C2): at most `MAX_FIGURES` per cell, at most `MAX_DIMENSION` px
on either side — the save DPI is lowered rather than the figure being cropped.
"""

from __future__ import annotations

import uuid
from pathlib import Path
from typing import Any

MAX_FIGURES = 8
MAX_DIMENSION = 2000


def collect(workspace: Path, plt: Any) -> tuple[list[dict[str, Any]], list[str]]:
    """Save and close every open figure. Returns (figures, warnings)."""
    figures: list[dict[str, Any]] = []
    warnings: list[str] = []
    if plt is None:
        return figures, warnings

    numbers = list(plt.get_fignums())
    if not numbers:
        return figures, warnings

    outputs = Path(workspace) / "outputs"
    outputs.mkdir(parents=True, exist_ok=True)

    for index, number in enumerate(numbers):
        fig = plt.figure(number)
        if index >= MAX_FIGURES:
            plt.close(fig)
            continue
        try:
            figures.append(_save(fig, outputs))
        except Exception as exc:  # noqa: BLE001 - a bad figure must not fail the cell
            warnings.append(f"figure {number} could not be saved: {exc}")
        finally:
            plt.close(fig)

    if len(numbers) > MAX_FIGURES:
        warnings.append(
            f"{len(numbers)} figures were open; only the first {MAX_FIGURES} were kept"
        )
    return figures, warnings


def _save(fig: Any, outputs: Path) -> dict[str, Any]:
    width_in, height_in = fig.get_size_inches()
    dpi = float(fig.dpi) or 100.0
    if width_in > 0 and height_in > 0:
        dpi = min(dpi, MAX_DIMENSION / width_in, MAX_DIMENSION / height_in)
    dpi = max(dpi, 1.0)

    name = f"fig-{uuid.uuid4()}.png"
    path = outputs / name
    # No bbox_inches="tight": the reported pixel size must match the file.
    fig.savefig(path, format="png", dpi=dpi)
    return {
        "artifact_rel": f"outputs/{name}",
        "width": int(round(width_in * dpi)),
        "height": int(round(height_in * dpi)),
    }
