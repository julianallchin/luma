"""Persistent Python execution kernel for Luma agent threads.

Modules:
    worker    NDJSON protocol loop + notebook-style cell execution (contract C2).
    bindings  Binding-manifest parsing into the immutable `luma` data plane (C1/C3).
    display   Bounded, notebook-style repr of a cell's last expression.
    figures   Matplotlib figure capture into the workspace output area.
    track     Staged, host-validated editing of the authored lighting timeline.

The worker and binding data plane remain domain-neutral. `track` is a small
Python domain facade over plain binding values and an injected host capability;
it knows nothing about Tauri, SQLite, or process transport.
"""

__all__ = ["bindings", "display", "figures", "track", "worker"]
