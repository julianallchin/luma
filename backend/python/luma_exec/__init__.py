"""Persistent Python execution kernel for Luma agent threads.

Modules:
    worker    NDJSON protocol loop + notebook-style cell execution (contract C2).
    bindings  Binding-manifest parsing into the immutable `luma` data plane (C1/C3).
    display   Bounded, notebook-style repr of a cell's last expression.
    music     The track as heard: felt tempo, listening views, onsets and MERT.
    figures   Matplotlib figure capture into the workspace output area.
    clip      Clip graph builders (bare names in every cell) and the shipped presets.
    score     Staged, host-checked editing of the track's score of clips.
    track     Errors, snapshots and window output shared by `score`.

The worker and binding data plane remain domain-neutral. `score` is a small
Python domain facade over plain binding values and an injected host capability;
it knows nothing about SQLite or process transport.
"""

__all__ = ["bindings", "clip", "display", "figures", "music", "score", "track", "worker"]
