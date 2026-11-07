"""A venue's Perform set: the patterns it plays live, and the pages that play them.

    p = luma.perform.add(preset("Wash"), name="Warm wash", selection="front_wash", at=(2, 0))
    luma.perform.update(p, graph=color(color="#ffb070", brightness=curve(control("drop"), "Ramp up")))
    luma.perform.play(p, level=0.5)
    print(luma.perform.describe())

A Perform pattern is a clip with no place on a timeline: the same graph,
selection, blend, seed and stack order (z, higher wins), built with the same
builders. A fader, live color or button node in its graph is a control of
that pattern, shown in its cell. Its pad is the button node its output's alpha depends on
(add() wires one in when the graph has none, starting on the next beat); the
pad's mode, start and envelope are that node's settings, so its fade is in the
graph, and the set scales it by the pattern's level times the master. A pad
that starts on the beat or bar waits for it, pressed or let go: live() lists
it as waiting until then. A pattern with no pad plays at its level whenever
the set is open.

The set is laid out on pages of a grid. A cell covers whole places, at
(column, row) counted from 0, and holds a pattern (every pattern has exactly
one cell), the master, tap, a fader of its own that moves several targets
together, a color cell: swatches, one of which is on, that fade the live
colors it targets to the chosen swatch, or a target: a venue group. A pad
pressed while targets are held plays its pattern on their groups instead of
its own selection, as an instance with a pad of its own; play(p, on=[...])
does the same. A page's one-at-a-time rows play one pattern at a time, per
set of groups.

Every verb is checked and saved at once, the same way the Perform tab saves,
and answers what it made. A refusal raises PerformRefused and saves nothing.
Bindings are hardware: each maps a MIDI or OSC input to a control.
"""
from __future__ import annotations

import copy
from dataclasses import dataclass
from typing import Any

from .clip import ClipError, Graph, _stop_color
from .host_errors import LumaHostCallError, PerformRefused
from .score import _graph, _plain
from .track import _blend

#: What a binding's hardware sends.
BINDING_MODES = ("fader", "momentary", "toggle")
#: What a color cell's fade is counted in.
FADE_UNITS = ("beats", "seconds")
#: Not given, where None means something: update_color(step=None) stops stepping.
_UNSET = object()


class PerformHostUnavailableError(RuntimeError):
    """This thread has no Perform set: chat from the Perform tab."""


@dataclass(frozen=True)
class Control:
    """A control a hand plays: its bank name, what it is called, and its kind:
    "level", "fader", "color" or "button" of a pattern (node is the graph
    node's id; its pad is a button; a color is a live color, which a color
    cell sets), "fader" of a fader cell, "swatch" of a color cell, "target"
    (held while pressed), "master" or "tap". mode is how hardware binds to it."""
    name: str
    label: str
    kind: str
    node: str | None = None
    #: The fader or color cell target this control is, for place(targets=...).
    _target: Any = None

    @property
    def mode(self):
        return "fader" if self.kind in ("level", "fader", "master") else "momentary"

    def __repr__(self):
        return f"<Control {self.label} ({self.kind}) {self.name}>"


@dataclass(frozen=True)
class Pattern:
    """One pattern of the set: id, name, selection, z, blend, seed, graph,
    and the cell it lives in. controls are its level and one per fader, live
    color or button node of its graph; pad is the button Control of its pad
    node, or None when its graph has no button on its output's alpha.
    graph.source() is Python that rebuilds the graph. Read-only: change it with
    luma.perform.update()."""
    id: str
    name: str
    selection: str
    z: int
    blend: str
    seed: int
    graph: Graph
    pad: "Control | None"
    cell: "Cell | None"
    controls: tuple

    def control(self, key):
        """A control of this pattern: "pad", "level", or a fader, live color
        or button node's id or label."""
        level, *nodes = self.controls
        if key == "level":
            return level
        if key == "pad" and self.pad is not None:
            return self.pad
        for control in nodes:
            if key in (control.node, control.label):
                return control
        if key == "pad":
            raise KeyError(f"pattern {self.name!r} has no pad: no button node feeds its output's "
                           "alpha, so it plays at its level whenever the set is open")
        raise KeyError(f"pattern {self.name!r} has no control {key!r}; it has "
                       + ", ".join(["level"] + (["pad"] if self.pad else []) + [c.node for c in nodes]))

    def __repr__(self):
        at = f" at {self.cell.at}" if self.cell else ""
        return f"<Pattern {self.name!r} {self.selection} z {self.z} {self.blend}{at} {self.id}>"


@dataclass(frozen=True)
class Cell:
    """A cell: id, page (its id), at (column, row from 0), size (width,
    height), kind ("pattern", "master", "tap", "fader", "color" or
    "target"), the pattern's id, and for a fader or color cell its label and
    targets. A color cell also has swatches ((r, g, b) each 0-1, linear Rec.
    2020), fade and unit (how long choosing one takes, in "beats" or
    "seconds"), and step, (bars, loops) or None: it moves on to the next
    swatch every bars bars. A target has a label and group, a venue group's
    name."""
    id: str
    page: str
    at: tuple
    size: tuple
    kind: str
    pattern: str | None
    label: str | None
    targets: tuple
    swatches: tuple = ()
    fade: float | None = None
    unit: str | None = None
    step: tuple | None = None
    group: str | None = None
    #: Each swatch's control name, in order; a target's one control.
    _swatches: tuple = ()

    def swatch(self, index):
        """Swatch index's control (from 0): pressed, it is chosen."""
        if not 0 <= index < len(self._swatches):
            raise KeyError(f"cell {self.label!r} has {len(self._swatches)} swatches; no swatch {index}")
        return Control(self._swatches[index], f"{self.label} {index + 1}", "swatch")

    def __repr__(self):
        what = {"pattern": f"pattern {self.pattern}", "fader": f"fader {self.label!r}",
                "color": f"color {self.label!r} of {len(self.swatches)} swatches",
                "target": f"target on {self.group}"}.get(self.kind, self.kind)
        return f"<Cell {what} at {self.at} size {self.size} {self.id}>"


@dataclass(frozen=True)
class Page:
    """A page: id, name, columns, rows, one_at_a_time (rows from 0) and its cells."""
    id: str
    name: str
    columns: int
    rows: int
    one_at_a_time: tuple
    cells: tuple

    def __repr__(self):
        return f"<Page {self.name!r} {self.columns}x{self.rows} cells={len(self.cells)} {self.id}>"


@dataclass(frozen=True)
class Binding:
    """Hardware bound to a control: id, the control's name, its label, the
    input address (MIDI CC or note with channel and number, or an OSC address
    and argument), mode ("fader", "momentary" or "toggle": what the hardware
    sends) and invert."""
    id: str
    control: str
    label: str
    address: dict
    mode: str
    invert: bool

    def __repr__(self):
        address = self.address
        if address.get("kind") == "osc":
            where = f"OSC {address.get('address')} arg {address.get('arg', 0)}"
        else:
            kind = "CC" if address.get("kind") == "midi_cc" else "note"
            where = f"MIDI {kind} {address.get('number')} ch {int(address.get('channel', 0)) + 1}"
        inverted = ", inverted" if self.invert else ""
        return f"<Binding {where} -> {self.label} ({self.mode}{inverted}) {self.id}>"


class Live:
    """What plays now: open (whether this venue's set is open, as it is while
    its Perform tab is shown), playing (pattern, level) pairs, instances
    (pattern, groups) pairs, each pattern playing on venue groups instead of
    its selection, waiting ((pattern, groups, "on" or "off") triples, each
    press of a pad that waits for its beat or bar to turn it on or off,
    groups () on its own selection), master, clock, decks, values (every
    control set, by name; a button is played, not set, so its pads count in
    playing instead), colors (each live color set, by name: the (r, g, b) it
    is at or fading to) and swatches (each color cell's swatch on, by cell
    id)."""

    def __init__(self, response, perform):
        self.open = bool(response.get("open"))
        state = response.get("state") or {}
        self.values = dict(state.get("controls") or {})
        self.colors = {name: tuple(color["fade"]["to"]) for name, color in (state.get("colors") or {}).items()}
        self.swatches = dict(state.get("swatches") or {})
        gates = {name: _gate(button) for name, button in (state.get("buttons") or {}).items()}
        on = {name for name, (lit, _) in gates.items() if lit}
        self.master = float(self.values.get(perform._controls["master"], 1.0))
        self.playing = tuple(
            (pattern, float(self.values.get(pattern.control("level").name, 1.0)))
            for pattern in perform.patterns
            if (pattern.pad.name in on if pattern.pad else self.open))
        patterns = {pattern.id: pattern for pattern in perform.patterns}
        instances = {name: (patterns[instance["patternId"]], tuple(instance["on"]))
                     for name, instance in (state.get("instances") or {}).items()
                     if instance["patternId"] in patterns}
        self.instances = tuple(played for name, played in instances.items() if name in on)
        plays = {**{pattern.pad.name: (pattern, ()) for pattern in perform.patterns if pattern.pad},
                 **instances}
        self.waiting = tuple((*plays[name], "off" if lit else "on")
                             for name, (lit, waits) in gates.items() if waits and name in plays)
        self.clock = state.get("clock")
        self.decks = tuple(state.get("decks") or ())

    def __repr__(self):
        if not self.open:
            return "<Live: this venue's Perform set is not open; it opens while its Perform tab is shown>"
        lines = [f"master {self.master:.2f}"]
        clock = self.clock or {}
        if clock.get("kind") == "tap":
            lines.append(f"clock: tapped tempo {float(clock.get('bpm', 0)):.1f} BPM")
        elif clock.get("kind") == "deck":
            lines.append(f"clock: deck {clock.get('deck')}")
        for deck in self.decks:
            state = "playing" if deck.get("playing") else "stopped"
            lines.append(f"deck {deck.get('id')}: {deck.get('title')!r} by {deck.get('artist')!r}, "
                         f"{float(deck.get('bpm', 0)):.1f} BPM, {state}")
        lines.append("playing: " + (", ".join(f"{p.name!r} at {level:.2f}" for p, level in self.playing)
                                    or "nothing"))
        if self.instances:
            lines.append("on groups: " + ", ".join(f"{p.name!r} on {' + '.join(on)}"
                                                    for p, on in self.instances))
        if self.waiting:
            lines.append("waiting for the beat or bar: " + ", ".join(
                f"{p.name!r}{' on ' + ' + '.join(on) if on else ''} to go {going}"
                for p, on, going in self.waiting))
        if self.swatches:
            lines.append("swatches on: " + ", ".join(f"cell {cell} swatch {index}"
                                                     for cell, index in self.swatches.items()))
        return "<Live\n  " + "\n  ".join(lines) + ">"


def _gate(button):
    """Whether a button the bank reports is on now, and whether a press of
    it waits for its beat or bar: its gate's times are seconds ago, less
    than 0 while still to come."""
    gate = button.get("gate")
    if not gate:
        return False, False
    on_for, off_for = gate["onFor"], gate.get("offFor")
    return (on_for >= 0 and (off_for is None or off_for < 0),
            on_for < 0 or (off_for is not None and off_for < 0))


def _id(value, kind):
    if isinstance(value, kind):
        return value.id
    if isinstance(value, str):
        return value
    raise TypeError(f"expected a {kind.__name__} or its id; got {value!r}")


def _swatches(swatches):
    out = []
    for swatch in swatches:
        try:
            rgb = _stop_color(swatch, "swatch")
        except ClipError as error:
            raise PerformRefused(str(error)) from None
        rgb = list(rgb) if isinstance(rgb, (list, tuple)) else rgb
        if not isinstance(rgb, list) or len(rgb) != 3:
            raise PerformRefused(f'a swatch is (r, g, b) or "#RRGGBB"; got {swatch!r}')
        out.append(rgb)
    return out


def _unit(unit):
    if unit not in FADE_UNITS:
        raise PerformRefused(f"a fade's unit is {' or '.join(FADE_UNITS)}; got {unit!r}")
    return unit


def _step(step):
    if step is None:
        return None
    if (isinstance(step, (str, bytes)) or len(step) != 2 or isinstance(step[0], bool)
            or not isinstance(step[0], int) or step[0] < 1):
        raise PerformRefused(f"step is (bars, loops), such as (4, True); got {step!r}")
    return {"bars": step[0], "loops": bool(step[1])}


def _place(value, name):
    if value is None:
        return None
    if isinstance(value, (str, bytes)) or len(value) != 2 or not all(
            isinstance(item, int) and not isinstance(item, bool) and item >= 0 for item in value):
        raise PerformRefused(f"{name} is two whole numbers from 0, such as (2, 0); got {value!r}")
    return [value[0], value[1]]


class Perform:
    """The Perform set of this thread's venue. See the module docstring."""

    __slots__ = ("_values", "_host_call", "_set", "_active")

    def __init__(self, values, *, host_call=None):
        object.__setattr__(self, "_values", values)
        object.__setattr__(self, "_host_call", host_call)
        object.__setattr__(self, "_set", _plain(values["set"]))
        object.__setattr__(self, "_active", True)

    def __setattr__(self, name, value):
        raise AttributeError("luma.perform is read-only; change it with its verbs")

    def _refresh(self, snapshot):
        for field in ("_values", "_host_call", "_set"):
            object.__setattr__(self, field, object.__getattribute__(snapshot, field))

    def _revoke(self):
        object.__setattr__(self, "_active", False)

    def _luma_catalog_items(self):
        values = object.__getattribute__(self, "_values")
        return [(key, values[key]) for key in values.keys()]

    @property
    def id(self):
        """The venue's id."""
        return str(object.__getattribute__(self, "_values")["venue_id"])

    # -- reading ---------------------------------------------------------

    @property
    def _controls(self):
        return self._set["controls"]

    @property
    def pages(self):
        """Every page, in order."""
        return tuple(Page(page["id"], page["name"], page["columns"], page["rows"],
                          tuple(page.get("oneAtATime") or ()),
                          tuple(self._cell(page["id"], cell) for cell in page["cells"]))
                     for page in self._set["layout"]["pages"])

    def _cell(self, page, cell):
        content = cell["content"]
        step = content.get("step")
        swatches = self._controls["cells"].get(cell["id"]) if content["kind"] == "color" else None
        if content["kind"] == "target":
            swatches = (self._controls["cells"][cell["id"]],)
        return Cell(cell["id"], page, (cell["column"], cell["row"]), (cell["width"], cell["height"]),
                    content["kind"], content.get("patternId"), content.get("label"),
                    tuple(copy.deepcopy(content.get("targets") or ())),
                    tuple(tuple(rgb) for rgb in content.get("swatches") or ()), content.get("fade"),
                    content.get("unit"), (step["bars"], step["loops"]) if step else None,
                    content.get("group"), tuple(swatches or ()))

    @property
    def cells(self):
        """Every cell, page by page."""
        return tuple(cell for page in self.pages for cell in page.cells)

    @property
    def patterns(self):
        """Every pattern, the top of the set first."""
        cells = {cell.pattern: cell for cell in self.cells if cell.pattern}
        return tuple(self._pattern(value, cells.get(value["id"])) for value in self._set["patterns"])

    def _pattern(self, value, cell):
        names = self._controls["patterns"].get(value["id"]) or {}
        controls = [Control(names["level"], "Level", "level",
                            _target={"kind": "level", "patternId": value["id"]})]
        for node in names.get("nodes", ()):
            target = {"kind": "node", "patternId": value["id"], "node": node["node"]}
            controls.append(Control(node["control"], node["label"], node["kind"], node["node"],
                                    target if node["kind"] in ("fader", "color") else None))
        pad = next((c for c in controls if c.node is not None and c.node == names.get("pad")), None)
        return Pattern(value["id"], value["name"], value["selection"]["expression"], value["zIndex"],
                       value["blendMode"], int(value["seed"]),
                       Graph.from_json(value["graph"], name=value["name"]),
                       pad, cell, tuple(controls))

    def pattern(self, key):
        """A pattern by id or exact name, or the pattern a Cell holds."""
        if isinstance(key, Pattern):
            key = key.id
        if isinstance(key, Cell):
            key = key.pattern
        found = [p for p in self.patterns if key in (p.id, p.name)]
        if len(found) != 1:
            names = ", ".join(repr(p.name) for p in self.patterns) or "none"
            raise KeyError(f"{'several patterns are' if found else 'no pattern is'} called {key!r}; "
                           f"the set has {names}")
        return found[0]

    def page(self, key=None):
        """A page by id, exact name or index from 0; the first with no key."""
        pages = self.pages
        if key is None:
            key = 0
        if isinstance(key, Page):
            key = key.id
        if isinstance(key, int) and not isinstance(key, bool):
            if 0 <= key < len(pages):
                return pages[key]
        for page in pages:
            if key in (page.id, page.name):
                return page
        raise KeyError(f"no page {key!r}; the set has {', '.join(repr(p.name) for p in pages)}")

    def cell(self, key):
        """A cell by id, or the cell a Pattern lives in."""
        if isinstance(key, Pattern):
            key = key.id
        for cell in self.cells:
            if key in (cell.id, cell.pattern):
                return cell
        raise KeyError(f"no cell {key!r}")

    def controls(self):
        """Every control the set plays under: each pattern's, each fader cell's,
        each color cell's swatches, each target's (held while pressed), the
        master and tap."""
        controls = [control for pattern in self.patterns for control in pattern.controls]
        for cell in self.cells:
            if cell.kind == "fader":
                controls.append(Control(self._controls["cells"][cell.id], cell.label or "Fader", "fader"))
            if cell.kind == "color":
                controls.extend(cell.swatch(index) for index in range(len(cell.swatches)))
            if cell.kind == "target":
                controls.append(Control(cell._swatches[0], cell.label or cell.group, "target"))
        controls.append(Control(self._controls["master"], "Master", "master"))
        controls.append(Control(self._controls["tap"], "Tap", "tap"))
        return tuple(controls)

    def describe(self):
        """The set as text: each page's cells by place, then the patterns."""
        patterns = {p.id: p for p in self.patterns}
        lines = [f"Perform set: {len(patterns)} patterns on {len(self.pages)} pages"]
        for page in self.pages:
            rows = f", one at a time: rows {', '.join(map(str, page.one_at_a_time))}" if page.one_at_a_time else ""
            lines.append(f"page {page.name!r} {page.columns} columns by {page.rows} rows{rows} ({page.id})")
            for cell in sorted(page.cells, key=lambda c: (c.at[1], c.at[0])):
                size = f" {cell.size[0]}x{cell.size[1]}" if cell.size != (1, 1) else ""
                if cell.kind == "pattern":
                    p = patterns.get(cell.pattern)
                    what = f"{p.name!r} on {p.selection}" if p else "a deleted pattern"
                elif cell.kind == "fader":
                    what = f"fader {cell.label!r} moving {len(cell.targets)} targets"
                elif cell.kind == "color":
                    step = (f", the next every {cell.step[0]} bars{', looping' if cell.step[1] else ''}"
                            if cell.step else "")
                    what = (f"color {cell.label!r} of {len(cell.swatches)} swatches setting "
                            f"{len(cell.targets)} targets, fading over {cell.fade} {cell.unit}{step}")
                elif cell.kind == "target":
                    named = f" labelled {cell.label!r}" if cell.label else ""
                    what = f"target on group {cell.group}{named}"
                else:
                    what = cell.kind
                lines.append(f"  {cell.at}{size}: {what} ({cell.id})")
        lines.append("patterns, top first:")
        for p in patterns.values():
            nodes = [c.label for c in p.controls if c.kind in ("fader", "color", "button")]
            extra = f", controls {', '.join(nodes)}" if nodes else ""
            pad = f"pad {p.pad.node}" if p.pad else "no pad"
            lines.append(f"  {p.name!r} on {p.selection}, z {p.z}, {p.blend}, {pad}{extra} ({p.id})")
        return "\n".join(lines)

    def __repr__(self):
        return f"<luma.perform patterns={len(self._set['patterns'])} pages={len(self.pages)}>"

    # -- the host --------------------------------------------------------

    def _call(self, method, payload):
        if not object.__getattribute__(self, "_active"):
            raise PerformHostUnavailableError("this Perform set is no longer in scope; use luma.perform")
        host_call = object.__getattribute__(self, "_host_call")
        if host_call is None:
            raise PerformHostUnavailableError("luma.perform needs Luma's host")
        try:
            return host_call(method, payload)
        except LumaHostCallError as error:
            if error.code in ("refused", "not_found"):
                raise PerformRefused(str(error)) from None
            raise

    def _change(self, method, payload):
        answer = self._call(method, payload)
        object.__setattr__(self, "_set", answer["set"])
        return answer

    # -- patterns --------------------------------------------------------

    def add(self, graph, *, name=None, selection="all", page=None, at=None, size=(1, 1),
            blend=None, z=None, seed=None):
        """Add a pattern in a new cell and return it.

        graph is a Graph from color(), aim(), strobe() or laser(), preset("Wash"),
        or a preset name; a preset also names it and gives its blend. selection
        is a group expression. The cell goes on page (the first by default) at
        (column, row) from 0, size (width, height), or in the first free place.
        It goes on top of the set unless z says. A graph with no pad (no button
        node on its output's alpha) gets one: a toggle button node named "pad"
        that starts on the next beat, through a curve into alpha. Its start and
        fades are that node's settings, so change them with button(...) in the
        graph.
        """
        graph = _graph(graph)
        name = name if name is not None else graph.name
        if not name:
            raise PerformRefused('a pattern needs a name, such as name="Warm wash"')
        payload = {"name": str(name), "selection": str(selection), "graph": graph.json(),
                   "blend": _blend(blend or graph.blend or "replace"), "size": _place(size, "size")}
        if page is not None:
            payload["page"] = self.page(page).id
        if at is not None:
            payload["at"] = _place(at, "at")
        if z is not None:
            payload["z"] = int(z)
        if seed is not None:
            payload["seed"] = int(seed)
        return self.pattern(self._change("perform.add", payload)["id"])

    def update(self, pattern, *, graph=None, name=None, selection=None, blend=None, z=None,
               seed=None):
        """Change what is given of a pattern and return it; the rest stays. A
        graph is stored as given: keep its pad (a button node on its output's
        alpha) or the pattern plays whenever the set is open."""
        payload = {"id": _id(pattern, Pattern)}
        if graph is not None:
            payload["graph"] = _graph(graph).json()
        if name is not None:
            payload["name"] = str(name)
        if selection is not None:
            payload["selection"] = str(selection)
        if blend is not None:
            payload["blend"] = _blend(blend)
        if z is not None:
            payload["z"] = int(z)
        if seed is not None:
            payload["seed"] = int(seed)
        return self.pattern(self._change("perform.update", payload)["id"])

    def remove(self, pattern):
        """Delete a pattern, its cell, and every fader and color cell's hold on it."""
        self._change("perform.remove", {"id": _id(pattern, Pattern)})

    def reorder(self, patterns):
        """Put these patterns on top of the set in this order, top first; the
        rest keep their order under them."""
        self._change("perform.reorder", {"ids": [_id(p, Pattern) for p in patterns]})
        return self.patterns

    # -- the layout ------------------------------------------------------

    def add_page(self, name, *, columns=8, rows=4):
        """Add a page at the end and return it. Pages have 1 to 16 columns and rows."""
        answer = self._change("perform.page", {"name": str(name), "columns": columns, "rows": rows})
        return self.page(answer["id"])

    def update_page(self, page, *, name=None, columns=None, rows=None, one_at_a_time=None):
        """Rename or resize a page, or say which of its rows play one pattern at
        a time (rows from 0). A size that would cut off a cell is refused."""
        payload = {"id": self.page(page).id}
        for key, value in (("name", name), ("columns", columns), ("rows", rows)):
            if value is not None:
                payload[key] = value
        if one_at_a_time is not None:
            payload["one_at_a_time"] = [int(row) for row in one_at_a_time]
        return self.page(self._change("perform.page", payload)["id"])

    def remove_page(self, page):
        """Remove a page that holds no patterns. A set keeps at least one."""
        self._change("perform.remove_page", {"id": self.page(page).id})

    def busk(self):
        """Add a busk page made from the venue's rig and return it: targets
        (all, the role groups, then groups named for a place), a base and an
        accent color cell, effects from presets with a pad, a live color and
        a speed each, movers' movement one at a time, and flash hits (strobe,
        blinder, white, black). Tap, the master, the effects' levels and their
        speed stand at the right. It is a new page, "Busk" or "Busk 2", of new
        patterns over the set's; nothing there changes."""
        return self.page(self._change("perform.busk", {})["id"])

    def place(self, kind, *, page=None, at=None, size=(1, 1), label=None, targets=(), swatches=(),
              fade=1, unit="beats", step=None, group=None):
        """Add a cell of kind "master", "tap", "fader", "color" or "target" and
        return it.

        A fader cell moves every one of targets together: patterns (their
        level), controls from pattern.controls of kind "fader", or the venue's
        own control names, such as "drop". A color cell holds swatches, each
        (r, g, b) 0-1 or "#RRGGBB"; choosing one fades every target (controls
        of kind "color") to it over fade, in unit ("beats" or "seconds").
        step=(bars, loops) moves it on to the next swatch every bars bars of
        the set's clock, after the last back to the first when loops. A
        target holds group, a venue group's name such as "front_wash": a pad
        pressed while it is held plays its pattern there. Its label is the
        group's unless given.
        """
        if kind == "fader":
            content = {"kind": "fader", "label": str(label or "Fader"), "targets": self._targets(targets)}
        elif kind == "color":
            content = {"kind": "color", "label": str(label or "Colors"),
                       "swatches": _swatches(swatches), "fade": float(fade), "unit": _unit(unit),
                       "targets": self._targets(targets, colors=True), "step": _step(step)}
        elif kind == "target":
            if not group:
                raise PerformRefused('a target holds a venue group, such as group="front_wash"')
            content = {"kind": "target", "label": str(label or ""), "group": str(group)}
        elif kind in ("master", "tap"):
            content = {"kind": kind}
        else:
            raise PerformRefused(f"place makes a master, tap, fader, color or target cell; got {kind!r}. "
                                 "A pattern's cell comes with add().")
        payload = {"content": content, "size": _place(size, "size")}
        if page is not None:
            payload["page"] = self.page(page).id
        if at is not None:
            payload["at"] = _place(at, "at")
        return self.cell(self._change("perform.place", payload)["id"])

    def _targets(self, targets, colors=False):
        out = []
        for target in targets:
            if colors:
                if not isinstance(target, Control) or target.kind != "color":
                    raise PerformRefused(f"a color cell sets live colors, controls of kind \"color\" "
                                         f"such as p.control(\"main\"); got {target!r}")
                out.append(dict(target._target))
                continue
            if isinstance(target, Pattern):
                target = target.control("level")
            if isinstance(target, Control):
                if target._target is None or target.kind == "color":
                    raise PerformRefused(f"{target.label} is a {target.kind}: a fader cell moves "
                                         "levels, fader nodes and the venue's own controls")
                out.append(dict(target._target))
            elif isinstance(target, str):
                out.append({"kind": "named", "name": target})
            else:
                raise TypeError(f"a fader target is a Pattern, a fader Control or a control name; got {target!r}")
        return out

    def move(self, cell, *, at=None, size=None, page=None):
        """Move a cell to (column, row), resize it, or take it to another page
        (its first free place there unless at says). Accepts a Pattern too."""
        cell = self.cell(cell.id if isinstance(cell, Cell) else cell)
        payload = {"id": cell.id}
        if at is not None:
            payload["at"] = _place(at, "at")
        if size is not None:
            payload["size"] = _place(size, "size")
        if page is not None:
            payload["page"] = self.page(page).id
        return self.cell(self._change("perform.cell", payload)["id"])

    def update_fader(self, cell, *, label=None, targets=None):
        """Change a fader cell's label or targets (see place)."""
        payload = {"id": _id(cell, Cell)}
        if label is not None:
            payload["label"] = str(label)
        if targets is not None:
            payload["targets"] = self._targets(targets)
        return self.cell(self._change("perform.cell", payload)["id"])

    def update_color(self, cell, *, label=None, swatches=None, targets=None, fade=None, unit=None,
                     step=_UNSET):
        """Change a color cell's label, swatches, targets, fade, unit or step
        (see place); step=None stops it stepping."""
        payload = {"id": _id(cell, Cell)}
        if label is not None:
            payload["label"] = str(label)
        if swatches is not None:
            payload["swatches"] = _swatches(swatches)
        if targets is not None:
            payload["targets"] = self._targets(targets, colors=True)
        if fade is not None:
            payload["fade"] = float(fade)
        if unit is not None:
            payload["unit"] = _unit(unit)
        if step is not _UNSET:
            payload["step"] = _step(step)
        return self.cell(self._change("perform.cell", payload)["id"])

    def update_target(self, cell, *, label=None, group=None):
        """Change a target's label or group (see place)."""
        payload = {"id": _id(cell, Cell)}
        if label is not None:
            payload["label"] = str(label)
        if group is not None:
            payload["group"] = str(group)
        return self.cell(self._change("perform.cell", payload)["id"])

    def remove_cell(self, cell):
        """Remove a master, tap, fader, color or target cell. A pattern's cell goes with remove()."""
        self._change("perform.remove_cell", {"id": _id(cell, Cell)})

    # -- playing ---------------------------------------------------------

    def live(self):
        """What plays now. Read it again to see a change."""
        return Live(self._call("perform.live", {}), self)

    def _name(self, control):
        if isinstance(control, Control):
            return control.name
        if isinstance(control, Pattern):
            return control.control("pad").name
        return str(control)

    def set(self, control, value):
        """Set a control 0-1, as a fader on screen does: a Control, a Pattern's
        pad, or a control name. A button set above 0.5 is on."""
        return Live(self._call("perform.control", {"name": self._name(control), "value": float(value)}), self)

    def press(self, control):
        """Press and release a pad or button, which plays by its mode."""
        name = self._name(control)
        self._call("perform.press", {"name": name, "down": True})
        return Live(self._call("perform.press", {"name": name, "down": False}), self)

    def choose(self, cell, index):
        """Choose swatch index (from 0) of a color cell, as a press on screen
        does: its targets fade to it."""
        return self.press(self.cell(_id(cell, Cell)).swatch(index))

    def _on(self, pattern, value, on):
        if isinstance(on, str):
            on = [on]
        groups = [str(group) for group in on]
        if not groups:
            raise PerformRefused('on names venue groups, such as on=["front_wash"]')
        answer = self._call("perform.control", {"name": pattern.control("pad").name,
                                                "value": value, "on": groups})
        return Live(answer, self)

    def play(self, pattern, level=None, on=None):
        """Turn a pattern's pad on, at level (0-1) when given, to hear it. A
        pattern with no pad already plays while the set is open. on=[groups]
        plays it on those venue groups instead of its selection, as targets
        held on screen do: an instance of its own, with its own pad."""
        pattern = self.pattern(pattern)
        if level is not None:
            self.set(pattern.control("level"), level)
        if on is not None:
            return self._on(pattern, 1.0, on)
        return self.live() if pattern.pad is None else self.set(pattern.pad, 1.0)

    def stop(self, pattern, on=None):
        """Turn a pattern's pad off, or with on=[groups] its instance there;
        it fades by the pad node's release. A pattern with no pad has nothing
        to turn off: set its level to 0."""
        pattern = self.pattern(pattern)
        if on is not None:
            return self._on(pattern, 0.0, on)
        return self.set(pattern.control("pad"), 0.0)

    # -- bindings --------------------------------------------------------

    def _bindings(self, answer):
        labels = {control.name: control.label for control in self.controls()}
        labels.update((control.name, f"{pattern.name} {control.label}")
                      for pattern in self.patterns for control in pattern.controls)
        return tuple(Binding(b["id"], b["control"], labels.get(b["control"], b["control"]),
                             dict(b["address"]), b["mode"], bool(b["invert"]))
                     for b in answer["inputs"]["bindings"])

    def bindings(self):
        """The hardware bound to this venue's controls."""
        return self._bindings(self._call("perform.bindings", {}))

    def update_binding(self, binding, *, mode=None, invert=None):
        """Change what a binding's hardware sends (mode) or invert it."""
        binding = binding if isinstance(binding, Binding) else next(
            (b for b in self.bindings() if b.id == binding), None)
        if binding is None:
            raise PerformRefused("no such binding")
        mode = binding.mode if mode is None else mode
        if mode not in BINDING_MODES:
            raise PerformRefused(f"binding mode {mode!r}: expected one of {', '.join(BINDING_MODES)}")
        answer = self._call("perform.binding", {"id": binding.id, "mode": mode,
                                                "invert": binding.invert if invert is None else bool(invert)})
        return next(b for b in self._bindings(answer) if b.id == binding.id)

    def unbind(self, binding):
        """Remove a binding."""
        self._call("perform.unbind", {"id": _id(binding, Binding)})

    def learn(self, control, mode=None):
        """Bind the next MIDI or OSC input heard to control: a Control, a
        Pattern's pad, or a control name. Waits up to 20 seconds for someone to
        move a fader or press a pad, so ask them to first. mode defaults to the
        control's own ("fader" for a fader, "momentary" for a pad)."""
        if mode is None:
            mode = control.mode if isinstance(control, Control) else (
                "momentary" if isinstance(control, Pattern) else "fader")
        if mode not in BINDING_MODES:
            raise PerformRefused(f"binding mode {mode!r}: expected one of {', '.join(BINDING_MODES)}")
        try:
            answer = self._call("perform.learn", {"control": self._name(control), "mode": mode})
        except LumaHostCallError as error:
            if error.code == "not_heard":
                raise PerformRefused(str(error)) from None
            raise
        return next(b for b in self._bindings(answer) if b.id == answer["binding"]["id"])
