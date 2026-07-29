"""Binding-manifest parsing and the read-only `luma` namespace.

Implements contract C1 (manifest JSON, written by Rust) and contract C3 (the
Python-side namespace, per design doc §7.3 / §8 / §10).

Manifest shape (C1, abridged):

    {
      "schema_version": 1,
      "revision": "r-<uuid>",
      "agent_kind": "track_copilot" | "pattern_graph",
      "scope": {"track_id":..., "window": {"start_s":..,"end_s":..} | null, ...},
      "root": <BindingValue>,
      "artifacts": {"<id>": {"kind","encoding","rel_path","byte_len",...}}
    }

BindingValue is one of:

    null / bool / number / string / array        -> plain Python value
    object without "$kind"                       -> record node (LumaRecord)
    {"$kind":"tensor", "artifact_id", "dtype", "shape", "byte_offset",
     "axes":[AxisSpec...], "unit", "provenance"} -> LumaTensor
    {"$kind":"unavailable", "reason": "..."}     -> Unavailable

AxisSpec is one of `linear`, `coordinates` (inline `values` or a nested
tensor ref), `labels`, `index`.

Everything artifact-backed is lazy: no bytes are read until `.values` (or
`np.asarray`) is touched. Materialized arrays are always read-only, either as
`np.memmap(mode="r")` or an ndarray with `writeable=False`.

Usage:

    ns = load_manifest(Path(workspace), "inputs/manifest-r-1.json")
    ns.features.beats.values      # read-only numpy array
    ns.catalog()                  # compact human-readable inventory
"""

from __future__ import annotations

import json
import os
from pathlib import Path
from typing import Any, Iterator, Mapping, Sequence

import numpy as np

# ---------------------------------------------------------------------------
# constants
# ---------------------------------------------------------------------------

SUPPORTED_SCHEMA_VERSION = 1

#: Manifest dtype token -> explicit little-endian numpy dtype string.
#: f16 stays f16 — never silently upcast, the agent may be counting bytes.
DTYPE_MAP: dict[str, str] = {
    "f16": "<f2",
    "f32": "<f4",
    "f64": "<f8",
    "i8": "|i1",
    "i16": "<i2",
    "i32": "<i4",
    "i64": "<i8",
    "u8": "|u1",
    "u16": "<u2",
    "u32": "<u4",
    "u64": "<u8",
    "bool": "|b1",
}

#: 18-byte Luma PCM header: u32 version | u32 sample_rate | u16 channels | u64 len.
PCM_HEADER_BYTES = 18

#: Arrays at or above this many bytes are memory-mapped rather than read.
MMAP_THRESHOLD_BYTES = 4 << 20


class LumaBindingError(RuntimeError):
    """The manifest is malformed or an artifact does not match its declaration."""


class LumaUnavailableError(RuntimeError):
    """A binding path exists in the schema but carries no data for this scope."""


# ---------------------------------------------------------------------------
# axes
# ---------------------------------------------------------------------------


class Axis:
    """One semantic axis of a `LumaTensor` (design §8.2).

    `values` materializes the axis coordinates lazily:
      - linear      -> start + step * arange(count)
      - coordinates -> the inline list, or the referenced tensor's values
      - labels      -> the label list (as an object array)
      - index       -> arange(count)
    """

    __slots__ = ("kind", "name", "unit", "_count", "_labels", "_spec", "_store", "_cache")

    def __init__(self, spec: Mapping[str, Any], store: "ArtifactStore") -> None:
        self.kind = str(spec.get("kind", "index"))
        self.name = str(spec.get("name", self.kind))
        self.unit = spec.get("unit")
        self._spec = spec
        self._store = store
        self._cache: np.ndarray | None = None
        self._labels: list[str] | None = None
        if self.kind == "labels":
            self._labels = [str(x) for x in spec.get("labels", [])]
            self._count = len(self._labels)
        elif self.kind == "coordinates":
            inline = spec.get("values")
            self._count = (
                len(inline)
                if inline is not None
                else int((spec.get("tensor") or {}).get("shape", [0])[0])
            )
        else:
            self._count = int(spec.get("count", 0))

    @property
    def count(self) -> int:
        return self._count

    @property
    def labels(self) -> list[str] | None:
        """Label strings for a `labels` axis, else None."""
        return list(self._labels) if self._labels is not None else None

    @property
    def values(self) -> np.ndarray:
        if self._cache is None:
            self._cache = self._materialize()
            self._cache.flags.writeable = False
        return self._cache

    def _materialize(self) -> np.ndarray:
        if self.kind == "linear":
            start = float(self._spec.get("start", 0.0))
            step = float(self._spec.get("step", 1.0))
            return start + step * np.arange(self._count, dtype=np.float64)
        if self.kind == "coordinates":
            inline = self._spec.get("values")
            if inline is not None:
                return np.asarray(inline, dtype=np.float64)
            ref = self._spec.get("tensor")
            if not ref:
                raise LumaBindingError(
                    f"coordinates axis {self.name!r} has neither values nor tensor"
                )
            return np.array(self._store.array_from_ref(ref), copy=True)
        if self.kind == "labels":
            return np.asarray(self._labels, dtype=object)
        return np.arange(self._count, dtype=np.int64)

    def describe(self) -> str:
        if self.kind == "linear":
            start = float(self._spec.get("start", 0.0))
            step = float(self._spec.get("step", 1.0))
            unit = f" {self.unit}" if self.unit else ""
            return f"{self.name}(linear {start:g}+{step:g}{unit} x{self._count})"
        if self.kind == "labels":
            shown = ",".join((self._labels or [])[:6])
            more = ",…" if self._labels and len(self._labels) > 6 else ""
            return f"{self.name}(labels[{shown}{more}])"
        if self.kind == "coordinates":
            unit = f" {self.unit}" if self.unit else ""
            return f"{self.name}(coords x{self._count}{unit})"
        return f"{self.name}(index x{self._count})"

    def __repr__(self) -> str:  # pragma: no cover - debugging aid
        return f"<Axis {self.describe()}>"


# ---------------------------------------------------------------------------
# artifact store
# ---------------------------------------------------------------------------


class ArtifactStore:
    """Resolves manifest artifact ids to lazily loaded, read-only arrays.

    Materialized arrays are cached by (artifact id, dtype, offset, shape) so two
    tensors pointing at the same bytes share one mapping (design §10.4).
    """

    def __init__(self, workspace: Path, artifacts: Mapping[str, Mapping[str, Any]]):
        self.workspace = Path(workspace)
        self.artifacts = dict(artifacts or {})
        self._cache: dict[tuple, np.ndarray] = {}

    def entry(self, artifact_id: str) -> Mapping[str, Any]:
        try:
            return self.artifacts[artifact_id]
        except KeyError:
            raise LumaBindingError(f"unknown artifact id {artifact_id!r}") from None

    def path(self, artifact_id: str) -> Path:
        rel = str(self.entry(artifact_id).get("rel_path", ""))
        if not rel:
            raise LumaBindingError(f"artifact {artifact_id!r} has no rel_path")
        if os.path.isabs(rel):
            raise LumaBindingError(f"artifact {artifact_id!r} rel_path must be relative")
        resolved = (self.workspace / rel).resolve()
        root = self.workspace.resolve()
        if not str(resolved).startswith(str(root) + os.sep):
            raise LumaBindingError(f"artifact {artifact_id!r} escapes the workspace")
        return resolved

    def array_from_ref(self, ref: Mapping[str, Any]) -> np.ndarray:
        """Load the array described by a bare TensorRef (`$kind` optional)."""
        return self.array(
            str(ref["artifact_id"]),
            str(ref.get("dtype", "f32")),
            [int(x) for x in ref.get("shape", [])],
            int(ref.get("byte_offset", 0) or 0),
        )

    def array(
        self,
        artifact_id: str,
        dtype: str,
        shape: Sequence[int],
        byte_offset: int,
    ) -> np.ndarray:
        key = (artifact_id, dtype, byte_offset, tuple(shape))
        hit = self._cache.get(key)
        if hit is not None:
            return hit
        arr = self._load(artifact_id, dtype, tuple(int(x) for x in shape), byte_offset)
        if arr.flags.writeable:
            arr.flags.writeable = False
        self._cache[key] = arr
        return arr

    def _load(
        self,
        artifact_id: str,
        dtype: str,
        shape: tuple[int, ...],
        byte_offset: int,
    ) -> np.ndarray:
        entry = self.entry(artifact_id)
        encoding = str(entry.get("encoding", "raw_le"))
        path = self.path(artifact_id)
        np_dtype = np.dtype(DTYPE_MAP.get(dtype, dtype))

        if encoding == "npy":
            if byte_offset:
                raise LumaBindingError(
                    f"artifact {artifact_id!r}: npy encoding requires byte_offset 0"
                )
            nbytes = int(np.prod(shape)) * np_dtype.itemsize if shape else 0
            mmap_mode = "r" if nbytes >= MMAP_THRESHOLD_BYTES else None
            arr = np.load(path, mmap_mode=mmap_mode, allow_pickle=False)
            if shape and tuple(arr.shape) != shape:
                raise LumaBindingError(
                    f"artifact {artifact_id!r}: npy shape {tuple(arr.shape)} "
                    f"!= manifest shape {shape}"
                )
            if arr.dtype != np_dtype:
                raise LumaBindingError(
                    f"artifact {artifact_id!r}: npy dtype {arr.dtype} "
                    f"!= manifest dtype {np_dtype}"
                )
            return arr

        if encoding == "pcm_f32":
            if byte_offset < PCM_HEADER_BYTES:
                raise LumaBindingError(
                    f"artifact {artifact_id!r}: pcm_f32 byte_offset {byte_offset} "
                    f"is inside the {PCM_HEADER_BYTES}-byte header"
                )
            if np_dtype != np.dtype("<f4"):
                raise LumaBindingError(
                    f"artifact {artifact_id!r}: pcm_f32 must declare dtype f32"
                )
            return self._read_raw(path, np_dtype, shape, byte_offset)

        if encoding == "raw_le":
            return self._read_raw(path, np_dtype, shape, byte_offset)

        raise LumaBindingError(
            f"artifact {artifact_id!r}: encoding {encoding!r} is not a numeric array"
        )

    @staticmethod
    def _read_raw(
        path: Path,
        np_dtype: np.dtype,
        shape: tuple[int, ...],
        byte_offset: int,
    ) -> np.ndarray:
        count = int(np.prod(shape)) if shape else 0
        nbytes = count * np_dtype.itemsize
        on_disk = path.stat().st_size
        if byte_offset + nbytes > on_disk:
            raise LumaBindingError(
                f"{path.name}: needs {byte_offset + nbytes} bytes, file has {on_disk}"
            )
        if nbytes >= MMAP_THRESHOLD_BYTES and byte_offset % np_dtype.itemsize == 0:
            return np.memmap(
                path, dtype=np_dtype, mode="r", offset=byte_offset, shape=shape
            )
        # Unaligned offsets (the 18-byte PCM header is not 4-aligned) and small
        # arrays go through an ordinary read.
        with open(path, "rb") as fh:
            fh.seek(byte_offset)
            arr = np.fromfile(fh, dtype=np_dtype, count=count)
        if arr.size != count:
            raise LumaBindingError(f"{path.name}: short read ({arr.size} of {count})")
        return arr.reshape(shape)


# ---------------------------------------------------------------------------
# nodes
# ---------------------------------------------------------------------------


class Unavailable:
    """A binding path that exists in the schema but has no data for this scope.

    Reprs fine, so `luma.track.key` is discoverable. Touching `.values` (or any
    other data accessor) raises `LumaUnavailableError` with the reason.
    """

    __slots__ = ("reason", "path")

    def __init__(self, reason: str, path: str = "luma") -> None:
        self.reason = str(reason)
        self.path = path

    def __repr__(self) -> str:
        return f"<unavailable {self.path}: {self.reason}>"

    def __bool__(self) -> bool:
        return False

    def _raise(self) -> "Unavailable":
        raise LumaUnavailableError(f"{self.path} is unavailable: {self.reason}")

    @property
    def values(self) -> np.ndarray:
        self._raise()
        raise AssertionError  # unreachable

    def __getattr__(self, name: str) -> "Unavailable":
        if name.startswith("__") and name.endswith("__"):
            raise AttributeError(name)
        # Sub-paths of an unavailable branch are unavailable for the same reason.
        return Unavailable(self.reason, f"{self.path}.{name}")

    def __getitem__(self, key: Any) -> "Unavailable":
        return Unavailable(self.reason, f"{self.path}[{key!r}]")

    def __array__(self, dtype: Any = None, copy: Any = None) -> np.ndarray:
        self._raise()
        raise AssertionError  # unreachable

    def keys(self) -> list[str]:
        return []


class LumaTensor:
    """Lazy, read-only numeric binding with semantic axes (design §8.1).

    `.values` is a read-only numpy array; `np.asarray(tensor)` works directly.
    The axis-derived conveniences (`times_s`, `primitive_ids`, `channels`,
    `frequencies_hz`) return None when the tensor has no such axis.
    """

    __slots__ = (
        "path",
        "artifact_id",
        "_dtype_token",
        "_shape",
        "_byte_offset",
        "axes",
        "unit",
        "provenance",
        "_store",
        "_extra",
        "_values",
    )

    def __init__(
        self,
        spec: Mapping[str, Any],
        store: ArtifactStore,
        path: str,
    ) -> None:
        self.path = path
        self._store = store
        self.artifact_id = str(spec["artifact_id"])
        self._dtype_token = str(spec.get("dtype", "f32"))
        self._shape = tuple(int(x) for x in spec.get("shape", []))
        self._byte_offset = int(spec.get("byte_offset", 0) or 0)
        self.unit = spec.get("unit")
        self.provenance = dict(spec.get("provenance") or {})
        self.axes = [Axis(a, store) for a in (spec.get("axes") or [])]
        if self.axes and len(self.axes) != len(self._shape):
            raise LumaBindingError(
                f"{path}: {len(self.axes)} axes for a {len(self._shape)}-d shape"
            )
        for axis, dim in zip(self.axes, self._shape):
            if axis.count and axis.count != dim:
                raise LumaBindingError(
                    f"{path}: axis {axis.name!r} has count {axis.count}, shape says {dim}"
                )
        self._extra = {
            k: v
            for k, v in spec.items()
            if k
            not in {
                "$kind",
                "artifact_id",
                "dtype",
                "shape",
                "byte_offset",
                "axes",
                "unit",
                "provenance",
            }
        }
        self._values: np.ndarray | None = None

    # -- data ---------------------------------------------------------------

    @property
    def values(self) -> np.ndarray:
        if self._values is None:
            self._values = self._store.array(
                self.artifact_id, self._dtype_token, self._shape, self._byte_offset
            )
        return self._values

    def __array__(self, dtype: Any = None, copy: Any = None) -> np.ndarray:
        arr = self.values
        if dtype is not None:
            arr = arr.astype(dtype, copy=False)
        return np.array(arr, copy=True) if copy else arr

    @property
    def shape(self) -> tuple[int, ...]:
        return self._shape

    @property
    def dtype(self) -> np.dtype:
        return np.dtype(DTYPE_MAP.get(self._dtype_token, self._dtype_token))

    @property
    def size(self) -> int:
        return int(np.prod(self._shape)) if self._shape else 0

    def __len__(self) -> int:
        return self._shape[0] if self._shape else 0

    # -- audio extras -------------------------------------------------------

    @property
    def sample_rate_hz(self) -> float | None:
        rate = self._extra.get("sample_rate_hz")
        if rate is None:
            rate = self._store.entry(self.artifact_id).get("sample_rate_hz")
        return float(rate) if rate is not None else None

    @property
    def stem_name(self) -> str | None:
        return self._extra.get("stem_name")

    # -- axis conveniences --------------------------------------------------

    def axis(self, name: str) -> Axis | None:
        for ax in self.axes:
            if ax.name == name:
                return ax
        return None

    @property
    def times_s(self) -> np.ndarray | None:
        ax = self.axis("time")
        if ax is None:
            for cand in self.axes:
                if cand.unit == "s":
                    ax = cand
                    break
        if ax is not None:
            return ax.values
        # Event tensors (beats, onsets) carry the seconds in the values.
        if self.unit == "s" and len(self._shape) == 1:
            return self.values
        return None

    @property
    def primitive_ids(self) -> list[str] | None:
        ax = self.axis("primitive")
        if ax is None:
            return None
        labels = ax.labels
        return labels if labels is not None else [str(v) for v in ax.values]

    @property
    def channels(self) -> list[str] | None:
        ax = self.axis("channel")
        if ax is None:
            return None
        labels = ax.labels
        return labels if labels is not None else [str(v) for v in ax.values]

    @property
    def frequencies_hz(self) -> np.ndarray | None:
        ax = self.axis("frequency")
        return ax.values if ax is not None else None

    # -- display ------------------------------------------------------------

    def describe(self) -> str:
        parts = [f"{self._dtype_token}{list(self._shape)}"]
        if self.unit:
            parts.append(f"unit={self.unit}")
        rate = self.sample_rate_hz
        if rate:
            parts.append(f"sr={rate:g}")
        if self.stem_name:
            parts.append(f"stem={self.stem_name}")
        if self.axes:
            parts.append("axes: " + ", ".join(a.describe() for a in self.axes))
        source = self.provenance.get("source")
        if source:
            version = self.provenance.get("processor_version")
            parts.append(f"src={source}" + (f"@{version}" if version is not None else ""))
        return "  ".join(parts)

    def __repr__(self) -> str:
        loaded = "loaded" if self._values is not None else "lazy"
        return f"<LumaTensor {self.path} {self.describe()} [{loaded}]>"


class LumaRecord:
    """A namespace node. Supports attribute access, dict access, and `.keys()`."""

    __slots__ = ("_items", "_path")

    def __init__(self, items: Mapping[str, Any], path: str = "luma") -> None:
        object.__setattr__(self, "_items", dict(items))
        object.__setattr__(self, "_path", path)

    # -- access -------------------------------------------------------------

    def __getattribute__(self, name: str) -> Any:
        # Bindings win over the dict-protocol helpers, so a node named e.g.
        # `get` is still reachable as an attribute. Use `_record_items()`
        # internally rather than `.items()` for that reason.
        if not name.startswith("_"):
            items = object.__getattribute__(self, "_items")
            if name in items:
                return items[name]
        return object.__getattribute__(self, name)

    def __getattr__(self, name: str) -> Any:
        if name.startswith("__") and name.endswith("__"):
            raise AttributeError(name)
        items = object.__getattribute__(self, "_items")
        if name in items:
            return items[name]
        path = object.__getattribute__(self, "_path")
        raise AttributeError(
            f"{path} has no binding {name!r}; available: {', '.join(sorted(items)) or '(none)'}"
        )

    def __getitem__(self, key: str) -> Any:
        items = object.__getattribute__(self, "_items")
        try:
            return items[key]
        except KeyError:
            path = object.__getattribute__(self, "_path")
            raise KeyError(
                f"{path} has no binding {key!r}; available: {', '.join(sorted(items)) or '(none)'}"
            ) from None

    def keys(self) -> list[str]:
        return list(object.__getattribute__(self, "_items").keys())

    def values(self) -> list[Any]:
        return list(object.__getattribute__(self, "_items").values())

    def items(self) -> list[tuple[str, Any]]:
        return list(object.__getattribute__(self, "_items").items())

    def get(self, key: str, default: Any = None) -> Any:
        return object.__getattribute__(self, "_items").get(key, default)

    def __contains__(self, key: object) -> bool:
        return key in object.__getattribute__(self, "_items")

    def __iter__(self) -> Iterator[str]:
        return iter(object.__getattribute__(self, "_items"))

    def __len__(self) -> int:
        return len(object.__getattribute__(self, "_items"))

    def __dir__(self) -> list[str]:
        return sorted(set(object.__getattribute__(self, "_items")) | {"keys", "items"})

    def __repr__(self) -> str:
        path = object.__getattribute__(self, "_path")
        items = object.__getattribute__(self, "_items")
        lines = [f"<{path}>"]
        for key, value in items.items():
            lines.append(f"  .{key}{_leaf_summary(value)}")
        return "\n".join(lines)


class LumaNamespace(LumaRecord):
    """The root `luma` object installed into the user namespace before each cell."""

    __slots__ = ("manifest", "store")

    def __init__(
        self,
        items: Mapping[str, Any],
        manifest: Mapping[str, Any],
        store: ArtifactStore,
    ) -> None:
        super().__init__(items, "luma")
        object.__setattr__(self, "manifest", dict(manifest))
        object.__setattr__(self, "store", store)

    @property
    def revision(self) -> str:
        return str(object.__getattribute__(self, "manifest").get("revision", ""))

    def catalog(self) -> str:
        """A compact multi-line inventory of every binding path (design §7.3)."""
        manifest = object.__getattribute__(self, "manifest")
        header = [
            f"luma binding revision {manifest.get('revision')} "
            f"(schema {manifest.get('schema_version')}, "
            f"agent_kind {manifest.get('agent_kind')})"
        ]
        scope = manifest.get("scope") or {}
        scope_bits = [f"{k}={v}" for k, v in scope.items() if v is not None and k != "window"]
        window = scope.get("window")
        if window:
            scope_bits.append(f"window={window.get('start_s')}..{window.get('end_s')}s")
        if scope_bits:
            header.append("scope: " + "  ".join(scope_bits))

        available: list[str] = []
        unavailable: list[str] = []
        _walk_catalog(self, "luma", available, unavailable)
        out = header + [""]
        out.append("available:")
        out.extend(available or ["  (none)"])
        if unavailable:
            out.append("")
            out.append("unavailable:")
            out.extend(unavailable)
        return "\n".join(out)

    def __repr__(self) -> str:
        return self.catalog()


def _record_items(record: "LumaRecord") -> dict[str, Any]:
    """The raw children of a record, bypassing binding/method name shadowing."""
    return object.__getattribute__(record, "_items")


def _leaf_summary(value: Any) -> str:
    if isinstance(value, LumaTensor):
        return f"  {value.describe()}"
    if isinstance(value, Unavailable):
        return f"  UNAVAILABLE: {value.reason}"
    if isinstance(value, LumaRecord):
        return "  {" + ", ".join(_record_items(value)) + "}"
    if isinstance(value, (list, tuple)):
        return f"  list[{len(value)}]"
    if isinstance(value, str):
        text = value if len(value) <= 40 else value[:37] + "..."
        return f"  = {text!r}"
    return f"  = {value!r}"


def _walk_catalog(
    node: Any,
    path: str,
    available: list[str],
    unavailable: list[str],
) -> None:
    if isinstance(node, Unavailable):
        unavailable.append(f"  {path:<40} {node.reason}")
        return
    if isinstance(node, LumaTensor):
        available.append(f"  {path:<40} {node.describe()}")
        return
    if isinstance(node, LumaRecord):
        for key, value in _record_items(node).items():
            _walk_catalog(value, f"{path}.{key}", available, unavailable)
        return
    if isinstance(node, (list, tuple)):
        available.append(f"  {path:<40} list[{len(node)}]")
        return
    if isinstance(node, str):
        text = node if len(node) <= 60 else node[:57] + "..."
        available.append(f"  {path:<40} {text!r}")
        return
    available.append(f"  {path:<40} {node!r}")


# ---------------------------------------------------------------------------
# manifest -> namespace
# ---------------------------------------------------------------------------


def _convert(value: Any, store: ArtifactStore, path: str) -> Any:
    if isinstance(value, dict):
        kind = value.get("$kind")
        if kind == "tensor":
            return LumaTensor(value, store, path)
        if kind == "unavailable":
            return Unavailable(str(value.get("reason", "no reason given")), path)
        if kind is not None:
            raise LumaBindingError(f"{path}: unknown $kind {kind!r}")
        return LumaRecord(
            {k: _convert(v, store, f"{path}.{k}") for k, v in value.items()}, path
        )
    if isinstance(value, list):
        return [_convert(v, store, f"{path}[{i}]") for i, v in enumerate(value)]
    return value


def build_namespace(manifest: Mapping[str, Any], workspace: Path) -> LumaNamespace:
    """Turn a parsed manifest into the `luma` root object."""
    version = int(manifest.get("schema_version", 0))
    if version != SUPPORTED_SCHEMA_VERSION:
        raise LumaBindingError(
            f"manifest schema_version {version} is not supported "
            f"(this worker speaks {SUPPORTED_SCHEMA_VERSION})"
        )
    store = ArtifactStore(workspace, manifest.get("artifacts") or {})
    root = manifest.get("root")
    if root is None:
        root = {}
    if not isinstance(root, dict) or root.get("$kind") is not None:
        raise LumaBindingError("manifest root must be a record object")

    items: dict[str, Any] = {
        k: _convert(v, store, f"luma.{k}") for k, v in root.items()
    }

    # The manifest envelope is authoritative for meta/window: `revision`,
    # `schema_version` and `agent_kind` always come from the top level, and the
    # window always mirrors scope.window. Anything else the provider put under
    # root.meta is preserved.
    scope = manifest.get("scope") or {}
    meta_items: dict[str, Any] = {}
    existing_meta = items.get("meta")
    if isinstance(existing_meta, LumaRecord):
        meta_items.update(_record_items(existing_meta))
    meta_items.update(
        {
            "schema_version": version,
            "revision": manifest.get("revision"),
            "agent_kind": manifest.get("agent_kind"),
            "scope": LumaRecord(
                {k: v for k, v in scope.items() if k != "window"}, "luma.meta.scope"
            ),
        }
    )
    items["meta"] = LumaRecord(meta_items, "luma.meta")

    window = scope.get("window")
    if isinstance(window, dict):
        items["window"] = LumaRecord(
            {
                "start_s": window.get("start_s"),
                "end_s": window.get("end_s"),
            },
            "luma.window",
        )
    elif "window" not in items:
        items["window"] = Unavailable("no analysis window in scope", "luma.window")

    return LumaNamespace(items, manifest, store)


def load_manifest(workspace: Path, manifest_rel: str) -> LumaNamespace:
    """Read `<workspace>/<manifest_rel>` and build the `luma` namespace."""
    workspace = Path(workspace)
    if os.path.isabs(manifest_rel):
        raise LumaBindingError("manifest_rel must be workspace-relative")
    path = (workspace / manifest_rel).resolve()
    if not str(path).startswith(str(workspace.resolve()) + os.sep):
        raise LumaBindingError("manifest_rel escapes the workspace")
    with open(path, "r", encoding="utf-8") as fh:
        manifest = json.load(fh)
    return build_namespace(manifest, workspace)
