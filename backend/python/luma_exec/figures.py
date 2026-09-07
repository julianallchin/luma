"""Figure capture (design §14.7).

A cell's *figures* are every image it produced, whatever produced them: a
matplotlib figure left open at the end of the cell, or a PNG the host rendered
during it (`luma.venue.render`). Both land in `<workspace>/outputs/` and are
reported to the host as `{"artifact_rel", "width", "height"}`, so the transcript,
the artifact store and the model-facing image parts see one kind of thing.

Because matplotlib figures are closed after each cell, "open at the end of the
cell" is exactly "created or touched by the cell"; a host figure is explicitly
registered at the moment it is produced, which is the same window.

Caps (contract C2): at most `MAX_FIGURES` per cell across *both* sources, at
most `MAX_DIMENSION` px on either side — the save DPI is lowered rather than the
figure being cropped.
"""

from __future__ import annotations

import uuid
from pathlib import Path
from typing import Any

MAX_FIGURES = 8
MAX_DIMENSION = 2000

OUTPUTS_DIR = "outputs"


class FigureSink:
    """One cell's figures, in the order they were produced.

    Lives for the process, not the cell: `collect` drains it, which is what
    makes "this cell's figures" well defined even though the `luma` namespace
    holding a reference to it is cached across cells.
    """

    def __init__(self) -> None:
        self._registered: list[dict[str, Any]] = []
        self._warnings: list[str] = []

    def register(self, artifact_rel: str, width: int, height: int) -> str:
        """Record a PNG the host already wrote under `outputs/`.

        Returns the path it will be reported under. Raises `ValueError` on a
        path that does not name a file in the workspace's output area — the
        host is trusted to render, not to choose where a figure lives.
        """
        rel = str(artifact_rel)
        parts = Path(rel).parts
        if len(parts) != 2 or parts[0] != OUTPUTS_DIR:
            raise ValueError(
                f"a figure path must be '{OUTPUTS_DIR}/<name>', not {rel!r}"
            )
        self._registered.append(
            {
                "artifact_rel": f"{OUTPUTS_DIR}/{parts[1]}",
                "width": int(width),
                "height": int(height),
            }
        )
        return rel

    def collect(self, workspace: Path, plt: Any) -> tuple[list[dict[str, Any]], list[str]]:
        """Drain the registered figures, then save and close every open one."""
        figures = self._registered
        warnings = self._warnings
        self._registered = []
        self._warnings = []

        numbers = list(plt.get_fignums()) if plt is not None else []
        produced = len(figures) + len(numbers)
        if numbers:
            outputs = Path(workspace) / OUTPUTS_DIR
            outputs.mkdir(parents=True, exist_ok=True)
            for number in numbers:
                fig = plt.figure(number)
                try:
                    # Every figure is closed, including the ones over the cap:
                    # a figure left open would be attributed to the next cell.
                    if len(figures) < MAX_FIGURES:
                        figures.append(_save(fig, outputs))
                except Exception as exc:  # noqa: BLE001 - a bad figure must not fail the cell
                    warnings.append(f"figure {number} could not be saved: {exc}")
                finally:
                    plt.close(fig)

        if produced > MAX_FIGURES:
            warnings.append(
                f"{produced} figures were produced; only the first {MAX_FIGURES} were kept"
            )
        return figures[:MAX_FIGURES], warnings


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
        "artifact_rel": f"{OUTPUTS_DIR}/{name}",
        "width": int(round(width_in * dpi)),
        "height": int(round(height_in * dpi)),
    }
