"""Persistent Python execution kernel for Luma agent threads.

Modules:
    worker    NDJSON protocol loop + notebook-style cell execution (contract C2).
    bindings  Binding-manifest parsing into the read-only `luma` namespace (C1/C3).
    display   Bounded, notebook-style repr of a cell's last expression.
    figures   Matplotlib figure capture into the workspace output area.

Nothing in this package knows about tracks, patterns, scores, venues, Tauri, or
SQLite; it only knows the manifest schema and the wire protocol.
"""

__all__ = ["bindings", "display", "figures", "worker"]
