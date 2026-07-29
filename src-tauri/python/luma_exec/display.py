"""Bounded, notebook-style rendering of a cell's last expression.

Design §14.8: never JSON-encode large numerical arrays for the model. A huge
array becomes a dtype/shape line plus NumPy's own summarized repr; every repr is
hard-capped with an explicit truncation marker.
"""

from __future__ import annotations

from typing import Any

#: Hard cap on a rendered repr, per contract C2.
REPR_LIMIT_BYTES = 8 * 1024

#: Arrays larger than this get a dtype/shape header plus a summarized repr.
ARRAY_SUMMARY_THRESHOLD = 64

_TRUNCATION_MARKER = "\n… [repr truncated at {limit} bytes]"


def render(value: Any, limit: int = REPR_LIMIT_BYTES) -> tuple[str, bool]:
    """Render `value` for the model. Returns (text, truncated)."""
    try:
        text = _render_inner(value)
    except BaseException as exc:  # noqa: BLE001 - a broken __repr__ must not kill the cell
        text = f"<unrenderable {type(value).__name__}: {type(exc).__name__}: {exc}>"
    return clamp(text, limit)


def clamp(text: str, limit: int = REPR_LIMIT_BYTES) -> tuple[str, bool]:
    """Clamp `text` to `limit` bytes, appending an explicit marker if cut."""
    encoded = text.encode("utf-8", "replace")
    if len(encoded) <= limit:
        return text, False
    head = encoded[:limit].decode("utf-8", "ignore")
    return head + _TRUNCATION_MARKER.format(limit=limit), True


def _render_inner(value: Any) -> str:
    import numpy as np

    from . import bindings

    if isinstance(value, bindings.LumaTensor):
        return repr(value)

    if isinstance(value, np.ndarray):
        return _render_array(value)

    if isinstance(value, np.generic):
        return repr(value)

    if isinstance(value, (list, tuple, set, frozenset)) and len(value) > 2000:
        kind = type(value).__name__
        return f"<{kind} of {len(value)} items; showing first 200>\n" + repr(
            list(value)[:200]
        )

    if isinstance(value, dict) and len(value) > 500:
        head = dict(list(value.items())[:100])
        return f"<dict of {len(value)} keys; showing first 100>\n{head!r}"

    return repr(value)


def _render_array(arr: Any) -> str:
    import numpy as np

    if arr.size <= ARRAY_SUMMARY_THRESHOLD:
        return repr(arr)
    header = f"ndarray dtype={arr.dtype} shape={tuple(arr.shape)}"
    with np.printoptions(threshold=32, edgeitems=3, precision=6, suppress=False):
        body = np.array2string(arr, separator=", ")
    return f"{header}\n{body}"
