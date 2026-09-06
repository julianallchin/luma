"""Small code-facing builder for the same lighting graphs the native app opens.

    p = luma.track.pattern("Soft chase")
    shape = p.node("soft_edges", softness=0.3)
    chase = p.node("chase", shape=shape.output("shape"), mapping="v")
    p.expose(chase, "width")
    p.check()
    pattern_id = p.save()
    edit = luma.track.edit()
    edit.add_clip(pattern_id, bars=(1, 5), z=0, selection="all", args={"width": 0.4})

Definitions (including the graphs inside Chase and Dissolve Flash) come from
Rust's catalogue. ``definition(name)`` follows those references. ``source()``
returns replayable Python for this draft. Saving uses authored history, and
returns an id usable immediately by the current track editor.
"""
from __future__ import annotations

import copy
import json
import re
from dataclasses import dataclass
from typing import Any

from .track import _items


def _plain(value):
    if isinstance(value, (list, tuple)):
        return [_plain(v) for v in value]
    if hasattr(value, "items"):
        return {k: _plain(v) for k, v in _items(value)}
    return value


def _wire(value_type, value):
    if value_type == "color" and isinstance(value, str) and re.fullmatch(r"#[0-9a-fA-F]{6}", value):
        return dict(zip(("r", "g", "b"), (int(value[i:i+2], 16) for i in (1, 3, 5))), a=1)
    # Native Color controls use RGB in 0..255. Typed engine values use 0..1.
    if value_type == "color" and isinstance(value, (list, tuple)):
        if len(value) != 3:
            raise ValueError("Color requires three RGB channels")
        return dict(zip(("r", "g", "b"), (c * 255 for c in value)), a=1)
    return copy.deepcopy(value)


ARG_TYPES = {"number": "Scalar", "color": "Color", "beats": "Beats",
             "proportion": "Proportion", "position": "Position", "boolean": "Boolean",
             "mapping": "Mapping", "boundary": "Boundary", "envelope": "Envelope"}


class SavedPattern(str):
    """A Pattern id carrying its new schema across persistent Python cells.

    It serializes as an ordinary id. A fresh Track can register it even when
    the draft was opened against an older manifest in a previous cell.
    """
    def __new__(cls, summary):
        value = super().__new__(cls, summary["id"])
        value._summary = copy.deepcopy(summary)
        return value

    def summary(self):
        return copy.deepcopy(self._summary)


@dataclass(frozen=True)
class Output:
    node: "Node"
    port: str


@dataclass(frozen=True)
class Node:
    _draft: "PatternDraft"
    id: str
    definition: str

    def output(self, port: str) -> Output:
        if port not in self._draft.definition(self.definition)["outputs"]:
            raise ValueError(f"{self.definition} has no output {port!r}")
        return Output(self, port)


class PatternDraft:
    def __init__(self, name, definitions, host_call, register, *, description=None):
        if not isinstance(name, str) or not name.strip():
            raise ValueError("Pattern needs a name")
        self.name, self.description = name, description
        self._definitions = _plain(definitions)
        self._host_call, self._register = host_call, register
        self._nodes = {}
        self._exposed = []
        self._saved = None

    def definition(self, name: str) -> dict:
        """Read a node's typed inputs, defaults, rates, outputs and implementation.

        A graph body references other definitions by name; call this again to
        follow one, exactly like opening a function's definition.
        """
        if name not in self._definitions:
            raise ValueError(f"Unknown node {name!r}; available: {', '.join(sorted(self._definitions))}")
        return copy.deepcopy(self._definitions[name])

    def _open(self):
        if self._saved is not None:
            raise ValueError("Pattern is already saved; start a new draft")

    def node(self, definition: str, *, id: str | None = None, **inputs) -> Node:
        self._open()
        spec = self.definition(definition)
        if id is None:
            index = 1
            id = f"{definition}_{index}"
            while id in self._nodes:
                index += 1
                id = f"{definition}_{index}"
        if not re.fullmatch(r"[a-z][a-z0-9_]*", id) or id == "pattern_args":
            raise ValueError("Node id must be snake_case and cannot be pattern_args")
        if id in self._nodes:
            raise ValueError(f"Duplicate node id {id!r}")
        for key, value in inputs.items():
            if key not in spec["inputs"]:
                raise ValueError(f"{definition} has no input {key!r}")
            if isinstance(value, Output):
                if value.node._draft is not self:
                    raise ValueError("Cannot connect nodes from different drafts")
                output = self.definition(value.node.definition)["outputs"][value.port]
                target = spec["inputs"][key]
                if output["value_type"] != target["value_type"]:
                    raise ValueError(f"{id}.{key} requires {target['value_type']}, got {output['value_type']}")
                if target["rate"] == "fixed" and output["rate"] != "fixed":
                    raise ValueError(f"{id}.{key} requires a fixed input")
        node = Node(self, id, definition)
        self._nodes[id] = (node, {k: v if isinstance(v, Output) else copy.deepcopy(v)
                                  for k, v in inputs.items()})
        return node

    def expose(self, node: Node, input: str, *, id: str | None = None, name: str | None = None):
        """Make an existing node input a per-clip control, preserving its default."""
        self._open()
        if node._draft is not self:
            raise ValueError("Node belongs to another draft")
        spec = self.definition(node.definition)["inputs"].get(input)
        if spec is None or spec["value_type"] not in ARG_TYPES:
            raise ValueError(f"{input!r} cannot be exposed as a clip control")
        id = id or input
        if not re.fullmatch(r"[a-z][a-z0-9_]*", id) or id == "selection":
            raise ValueError("Control id must be snake_case; selection is reserved")
        if any(e[2] == id or e[:2] == (node.id, input) for e in self._exposed):
            raise ValueError("Control or node input is already exposed")
        value = self._nodes[node.id][1].get(input)
        if isinstance(value, Output):
            raise ValueError("Connected inputs cannot also be clip controls")
        if input not in self._nodes[node.id][1] and spec["default"] is None:
            raise ValueError("Set a default value before exposing this input")
        suffix = {"beats": " (beats)", "proportion": " (0–1)"}.get(spec["value_type"], "")
        self._exposed.append((node.id, input, id, name or spec["name"] + suffix))

    def graph(self) -> dict:
        """Export the canonical native graph wire format (without writing it)."""
        nodes, edges, args = [], [], []
        for index, (node, inputs) in enumerate(self._nodes.values()):
            spec = self.definition(node.definition)
            params = {}
            for key, value in inputs.items():
                if isinstance(value, Output):
                    edges.append(self._edge(value.node.id, value.port, node.id, key))
                else:
                    params[key] = _wire(spec["inputs"][key]["value_type"], value)
            nodes.append({"id": node.id, "typeId": f"lighting/{node.definition}",
                          "params": params, "positionX": index * 300., "positionY": 0.})
        for node_id, key, arg_id, label in self._exposed:
            node, values = self._nodes[node_id]
            spec = self.definition(node.definition)["inputs"][key]
            value = values[key] if key in values else spec["default"]["value"]
            args.append({"id": arg_id, "name": label, "argType": ARG_TYPES[spec["value_type"]],
                         "defaultValue": _wire(spec["value_type"], value)})
            edges.append(self._edge("pattern_args", arg_id, node_id, key))
            next(n for n in nodes if n["id"] == node_id)["params"].pop(key, None)
        # Selection belongs to the Pattern's output, not its internal mask math.
        terminals = [(node.id, port) for node, _ in self._nodes.values()
                     for port, spec in self.definition(node.definition)["outputs"].items()
                     if spec["value_type"] == "lighting"
                     and not any(e["fromNode"] == node.id and e["fromPort"] == port for e in edges)]
        if len(terminals) != 1:
            raise ValueError("Pattern needs one Lighting output; combine outputs with add_lighting")
        args.append({"id": "selection", "name": "Selection", "argType": "Selection",
                     "defaultValue": {"expression": "all"}})
        edges.append(self._edge("pattern_args", "selection", terminals[0][0], "selection"))
        nodes.insert(0, {"id": "pattern_args", "typeId": "pattern_args", "params": {},
                         "positionX": -300., "positionY": 0.})
        return {"nodes": nodes, "edges": edges, "args": args}

    @staticmethod
    def _edge(source, port, target, input):
        return {"id": f"{target}-{input}", "fromNode": source, "fromPort": port,
                "toNode": target, "toPort": input}

    def source(self) -> str:
        """Replayable Python: literals, function-like nodes and exposed controls."""
        lines = [f"p = luma.track.pattern({self.name!r}, description={self.description!r})"]
        aliases = {id: f"n{index}" for index, id in enumerate(self._nodes)}
        for node, inputs in self._nodes.values():
            values = {key: (f"{aliases[value.node.id]}.output({value.port!r})" if isinstance(value, Output)
                            else repr(value)) for key, value in inputs.items()}
            kwargs = "".join(f", {key}={value}" for key, value in values.items())
            lines.append(f"{aliases[node.id]} = p.node({node.definition!r}, id={node.id!r}{kwargs})")
        for node, key, id, name in self._exposed:
            lines.append(f"p.expose({aliases[node]}, {key!r}, id={id!r}, name={name!r})")
        return "\n".join(lines) + "\n"

    def _request(self):
        payload = {"name": self.name, "description": self.description, "graph": self.graph()}
        # Catch NaN/inf and non-JSON values before invoking the worker protocol.
        json.dumps(payload, allow_nan=False)
        if self._host_call is None:
            raise RuntimeError("Pattern checks and saves need the Luma host")
        return payload

    def check(self):
        return self._host_call("track.pattern_check", self._request())

    def save(self) -> str:
        """Save to this score and register it for immediate edit.add_clip use."""
        if self._saved is None:
            result = self._host_call("track.pattern_create", self._request())
            self._register(result)
            self._saved = SavedPattern(result)
        return self._saved
