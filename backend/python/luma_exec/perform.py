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
one cell), a group (a venue group to choose), a list (colors or numbers, one
of which is on), a fader of its own that moves several targets together, or a
utility: the master, tap, Select or Clear. A page's one-at-a-time rows play
one pattern at a time where each plays.

Choose where, then do something; nothing chosen means everything. A group
tapped is chosen and stays so (choose() does the same). A pad tapped with
nothing chosen turns its pattern on or off on its own selection; with groups
chosen it comes on there, or if it already plays there leaves them, or adds
them. A list value tapped sets the list's targets; a color list with no
targets sets the base color of the chosen groups' lights (base() does the
same): color only, under every effect, lighting nothing itself. An effect
with blend "below" takes the color under it; one with its own color uses its
live color and a normal blend. select() (or Select held on screen) chooses
effects: the next color tapped on a list with no targets is their own, and
"below" puts them back on the color below. clear() chooses nothing;
clear(effects=True) lets every effect go.

Every verb is checked and saved at once, the same way the Perform tab saves,
and answers what it made. A refusal raises PerformRefused and saves nothing.
Bindings are hardware: each maps a MIDI or OSC input to a control, and
belongs to a controller. A controller is a device: the MIDI port it matches
and a profile (plain data: where its pads and faders sit on its grid, and
how it shows a color). Its lights show what each bound control does now;
map_page() puts a page's cells on its grid.
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
#: How a controller's light shines.
SHINES = ("steady", "pulse", "blink")
#: What a list's fade is counted in.
FADE_UNITS = ("beats", "seconds")
#: Not given, where None means something: update_list(step=None) stops stepping.
_UNSET = object()


class PerformHostUnavailableError(RuntimeError):
    """This thread has no Perform set: chat from the Perform tab."""


@dataclass(frozen=True)
class Control:
    """A control a hand plays: its bank name, what it is called, and its kind:
    "level", "fader", "color" or "button" of a pattern (node is the graph
    node's id; its pad is a button; a color is a live color, which a list
    sets), "fader" of a fader cell, "value" or "next" of a list, "group",
    "select", "clear", "master" or "tap". mode is how hardware binds to it."""
    name: str
    label: str
    kind: str
    node: str | None = None
    #: The fader or list target this control is, for place(targets=...).
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
    height), kind ("pattern", "group", "list", "fader", "master", "tap",
    "select" or "clear"), the pattern's id, and for a fader or list its label
    and targets. A list also has values (each (r, g, b) 0-1 in linear Rec.
    2020, a number 0-1, or "below", the color below), fade and unit (how long
    a color takes to fade, in "beats" or "seconds"), and step, (bars, loops)
    or None: it moves on to the next value every bars bars. A group cell has
    a label and group, a venue group's name."""
    id: str
    page: str
    at: tuple
    size: tuple
    kind: str
    pattern: str | None
    label: str | None
    targets: tuple
    values: tuple = ()
    fade: float | None = None
    unit: str | None = None
    step: tuple | None = None
    group: str | None = None
    #: Each value's control name, in order, then the list's own (its next
    #: value); a fader or group cell's one control.
    _controls: tuple = ()

    def value(self, index):
        """Value index's control (from 0): pressed, it is tapped."""
        if not 0 <= index < len(self.values):
            raise KeyError(f"cell {self.label!r} has {len(self.values)} values; no value {index}")
        return Control(self._controls[index], f"{self.label} {index + 1}", "value")

    @property
    def next(self):
        """A list's own control: pressed, its next value is tapped."""
        if self.kind != "list":
            raise KeyError(f"cell {self.id} is a {self.kind}, not a list")
        return Control(self._controls[-1], f"{self.label} next", "next")

    def __repr__(self):
        what = {"pattern": f"pattern {self.pattern}", "fader": f"fader {self.label!r}",
                "list": f"list {self.label!r} of {len(self.values)} values",
                "group": f"group {self.group}"}.get(self.kind, self.kind)
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
    sends), invert, and the id of the controller it belongs to."""
    id: str
    control: str
    label: str
    address: dict
    mode: str
    invert: bool
    controller: str = ""

    def __repr__(self):
        address = self.address
        if address.get("kind") == "osc":
            where = f"OSC {address.get('address')} arg {address.get('arg', 0)}"
        else:
            kind = "CC" if address.get("kind") == "midi_cc" else "note"
            where = f"MIDI {kind} {address.get('number')} ch {int(address.get('channel', 0)) + 1}"
        inverted = ", inverted" if self.invert else ""
        return f"<Binding {where} -> {self.label} ({self.mode}{inverted}) {self.id}>"


@dataclass(frozen=True)
class Controller:
    """A device: id, name, the MIDI port it matches ("" until one is heard,
    when it hears every input), whether that port is open now, and its
    profile, a dict:

        {"controls": [{"address": {"kind": "midi_note", "channel": 0, "number": 81},
                       "column": 0, "row": 0},
                      {"address": {"kind": "midi_cc", "channel": 0, "number": 48},
                       "column": 0, "row": 8, "fader": True}],
         "color": {"palette": {"0": [0, 0, 0], "5": [255, 0, 0]}},
         "modes": {"steady": {"channel": 0}, "blink": {"channel": 1}, "pulse": {"channel": 2}},
         "dim": 0.15}

    controls place each pad or fader on the device's grid, column and row
    from the top left. color is a palette (each velocity it takes, to the
    rgb 0-255 it shows) or {"sysex": "F0 ... {number} {r} {g} {b} F7"} for
    full rgb (each 0-127); with none, a light is the control's own note at
    127 lit, 0 dark. modes say how to shine: the channel to send on and an
    offset added to the velocity; a blink a device lacks is drawn by turning
    it on and off. dim is how bright a dim light is, 0-1."""
    id: str
    name: str
    port: str
    connected: bool
    profile: dict

    def __repr__(self):
        state = "connected" if self.connected else "not connected"
        return f"<Controller {self.name!r} port={self.port!r} {state} controls={len(self.profile.get('controls', []))} {self.id}>"


class Live:
    """What plays now: open (whether this venue's set is open, as it is while
    its Perform tab is shown), playing (pattern, level) pairs, effects
    ((pattern, groups, below) triples: the groups each plays on, () for its
    own selection, and whether it takes the color below), waiting ((pattern, "on"
    or "off") pairs, each press of a pad that waits for its beat or bar),
    chosen and held (the groups chosen, and those held down), selecting
    (whether Select is down or latched) and selected (the patterns it chose),
    master, clock, decks, values (every control set, by name; a button is
    played, not set, so its pads count in playing instead), colors (each live
    color set, by name: the (r, g, b) it is at or fading to), lists (each
    list's value on, by cell id) and base (each group's base color now)."""

    def __init__(self, response, perform):
        self.open = bool(response.get("open"))
        state = response.get("state") or {}
        self.values = dict(state.get("controls") or {})
        self.colors = {name: tuple(color["fade"]["to"]) for name, color in (state.get("colors") or {}).items()}
        self.lists = dict(state.get("lists") or {})
        self.chosen = tuple(state.get("chosen") or ())
        self.held = tuple(state.get("held") or ())
        self.selecting = bool(state.get("selecting"))
        self.base = {group: tuple(rgb) for group, rgb in (state.get("base") or {}).items()}
        gates = {name: _gate(button) for name, button in (state.get("buttons") or {}).items()}
        on = {name for name, (lit, _) in gates.items() if lit}
        self.master = float(self.values.get(perform._controls["master"], 1.0))
        patterns = {pattern.id: pattern for pattern in perform.patterns}
        self.selected = tuple(patterns[id] for id in state.get("selected") or () if id in patterns)
        self.effects = tuple((patterns[id], tuple(effect.get("groups") or ()), bool(effect.get("below")))
                             for id, effect in (state.get("effects") or {}).items() if id in patterns)
        self.playing = tuple(
            (pattern, float(self.values.get(pattern.control("level").name, 1.0)))
            for pattern in perform.patterns
            if (pattern.pad.name in on if pattern.pad else self.open))
        pads = {pattern.pad.name: pattern for pattern in perform.patterns if pattern.pad}
        self.waiting = tuple((pads[name], "off" if lit else "on")
                             for name, (lit, waits) in gates.items() if waits and name in pads)
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
        effects = {p.id: (groups, below) for p, groups, below in self.effects}

        def where(pattern):
            groups, below = effects.get(pattern.id, ((), False))
            return (f" on {' + '.join(groups)}" if groups else "") + (" (color below)" if below else "")
        lines.append("playing: " + (", ".join(f"{p.name!r}{where(p)} at {level:.2f}" for p, level in self.playing)
                                    or "nothing"))
        lines.append("chosen: " + (", ".join(self.chosen) or "nothing (everything)"))
        if self.held:
            lines.append("held: " + ", ".join(self.held))
        if self.selecting or self.selected:
            lines.append("selected: " + (", ".join(repr(p.name) for p in self.selected) or "nothing yet"))
        if self.waiting:
            lines.append("waiting for the beat or bar: " + ", ".join(
                f"{p.name!r} to go {going}" for p, going in self.waiting))
        if self.lists:
            lines.append("lists on: " + ", ".join(f"cell {cell} value {index}"
                                                  for cell, index in self.lists.items()))
        if self.base:
            lines.append("base color: " + ", ".join(
                f"{group} ({', '.join(f'{c:.2f}' for c in rgb)})" for group, rgb in self.base.items()))
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


def _color(color, what):
    try:
        rgb = _stop_color(color, what)
    except ClipError as error:
        raise PerformRefused(str(error)) from None
    rgb = list(rgb) if isinstance(rgb, (list, tuple)) else rgb
    if not isinstance(rgb, list) or len(rgb) != 3:
        raise PerformRefused(f'a {what} is (r, g, b) or "#RRGGBB"; got {color!r}')
    return rgb


def _values(values):
    """A list's values as the host takes them: a number 0-1, "below", or a color."""
    out = []
    for value in values:
        if isinstance(value, (int, float)) and not isinstance(value, bool):
            out.append({"number": float(value)})
        elif value == "below":
            out.append("below")
        else:
            out.append({"color": _color(value, "list value")})
    return out


def _colors(values):
    """Whether stored list values are colors (or the color below)."""
    return bool(values) and (values[0] == "below" or "color" in values[0])


def _value(value):
    """One stored list value as Python reads it."""
    if isinstance(value, dict) and "color" in value:
        return tuple(value["color"])
    if isinstance(value, dict) and "number" in value:
        return float(value["number"])
    return value


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
        names = self._controls["cells"].get(cell["id"])
        if content["kind"] == "list":
            names = (*names["values"], names["next"])
        elif names is not None:
            names = (names,)
        return Cell(cell["id"], page, (cell["column"], cell["row"]), (cell["width"], cell["height"]),
                    content["kind"], content.get("patternId"), content.get("label"),
                    tuple(copy.deepcopy(content.get("targets") or ())),
                    tuple(_value(value) for value in content.get("values") or ()), content.get("fade"),
                    content.get("unit"), (step["bars"], step["loops"]) if step else None,
                    content.get("group"), tuple(names or ()))

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
        """Every control the set plays under: each pattern's, each fader and
        group cell's, each list's values and its next, the master, tap,
        Select and Clear."""
        controls = [control for pattern in self.patterns for control in pattern.controls]
        for cell in self.cells:
            if cell.kind == "fader":
                controls.append(Control(cell._controls[0], cell.label or "Fader", "fader"))
            if cell.kind == "group":
                controls.append(Control(cell._controls[0], cell.label or cell.group, "group"))
            if cell.kind == "list":
                controls.extend(cell.value(index) for index in range(len(cell.values)))
                controls.append(cell.next)
        for kind in ("master", "tap", "select", "clear"):
            controls.append(Control(self._controls[kind], kind.capitalize(), kind))
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
                elif cell.kind == "list":
                    step = (f", the next every {cell.step[0]} bars{', looping' if cell.step[1] else ''}"
                            if cell.step else "")
                    colors = any(not isinstance(v, float) for v in cell.values)
                    sets = (f"setting {len(cell.targets)} targets" if cell.targets
                            else "setting the base color" if colors else "setting nothing")
                    what = (f"list {cell.label!r} of {len(cell.values)} {'colors' if colors else 'numbers'} "
                            f"{sets}, fading over {cell.fade} {cell.unit}{step}")
                elif cell.kind == "group":
                    named = f" labelled {cell.label!r}" if cell.label else ""
                    what = f"group {cell.group}{named}"
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
        """Delete a pattern, its cell, and every fader and list's hold on it."""
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
        """Add a busk page made from the venue's rig and return it: groups to
        choose (all, the role groups, then groups named for a place), a base
        color list (no targets: it colors the groups chosen, and ends with
        "below"), an accent color list setting the accent effects' own colors,
        effects from presets with a pad, a live color and a speed each (the
        base ones take the color below, the accent ones their own), movers'
        movement one at a time, and flash hits (strobe, blinder, white,
        black). Select, Clear, tap, the master, the effects' levels and their
        speed stand at the right. It is a new page, "Busk" or "Busk 2", of new
        patterns over the set's; nothing there changes."""
        return self.page(self._change("perform.busk", {})["id"])

    def place(self, kind, *, page=None, at=None, size=(1, 1), label=None, targets=(), values=(),
              fade=1, unit="beats", step=None, group=None):
        """Add a cell of kind "group", "list", "fader", "master", "tap",
        "select" or "clear" and return it.

        A group cell holds group, a venue group's name such as "front_wash" or
        "all": tapped, it is chosen. Its label is the group's unless given. A
        list holds values of one kind: colors ((r, g, b) 0-1 or "#RRGGBB"),
        numbers 0-1, or "below" (the color below, for a color list with no
        targets). A value tapped sets every target to it: controls of kind
        "color" for colors, fading over fade in unit ("beats" or "seconds"),
        or what a fader moves for numbers. A color list with no targets sets
        the base color of the groups chosen, or after select() the effects'
        own colors. step=(bars, loops) moves it on to the next value every bars
        bars of the set's clock, after the last back to the first when loops.
        A fader cell moves every one of targets together: patterns (their
        level), controls of kind "fader", or the venue's own control names,
        such as "drop".
        """
        if kind == "fader":
            content = {"kind": "fader", "label": str(label or "Fader"), "targets": self._targets(targets)}
        elif kind == "list":
            stored = _values(values)
            colors = _colors(stored)
            content = {"kind": "list", "label": str(label or ("Colors" if colors else "Values")),
                       "values": stored, "fade": float(fade), "unit": _unit(unit),
                       "targets": self._targets(targets, colors=colors), "step": _step(step)}
        elif kind == "group":
            if not group:
                raise PerformRefused('a group cell holds a venue group, such as group="front_wash"')
            content = {"kind": "group", "label": str(label or ""), "group": str(group)}
        elif kind in ("master", "tap", "select", "clear"):
            content = {"kind": kind}
        else:
            raise PerformRefused(f"place makes a group, list, fader, master, tap, select or clear cell; "
                                 f"got {kind!r}. A pattern's cell comes with add().")
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
                    raise PerformRefused(f"a color list sets live colors, controls of kind \"color\" "
                                         f"such as p.control(\"main\"); got {target!r}")
                out.append(dict(target._target))
                continue
            if isinstance(target, Pattern):
                target = target.control("level")
            if isinstance(target, Control):
                if target._target is None or target.kind == "color":
                    raise PerformRefused(f"{target.label} is a {target.kind}: a fader or number list "
                                         "moves levels, fader nodes and the venue's own controls")
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

    def update_list(self, cell, *, label=None, values=None, targets=None, fade=None, unit=None,
                    step=_UNSET):
        """Change a list's label, values, targets, fade, unit or step (see
        place); step=None stops it stepping."""
        cell = self.cell(_id(cell, Cell))
        payload = {"id": cell.id}
        if label is not None:
            payload["label"] = str(label)
        if values is not None:
            payload["values"] = _values(values)
        colors = _colors(payload.get("values") or _values(cell.values))
        if targets is not None:
            payload["targets"] = self._targets(targets, colors=colors)
        if fade is not None:
            payload["fade"] = float(fade)
        if unit is not None:
            payload["unit"] = _unit(unit)
        if step is not _UNSET:
            payload["step"] = _step(step)
        return self.cell(self._change("perform.cell", payload)["id"])

    def update_group(self, cell, *, label=None, group=None):
        """Change a group cell's label or group (see place)."""
        payload = {"id": _id(cell, Cell)}
        if label is not None:
            payload["label"] = str(label)
        if group is not None:
            payload["group"] = str(group)
        return self.cell(self._change("perform.cell", payload)["id"])

    def remove_cell(self, cell):
        """Remove a cell that holds no pattern. A pattern's cell goes with remove()."""
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

    def tap(self, cell, index):
        """Tap value index (from 0) of a list, as a press on screen does."""
        return self.press(self.cell(_id(cell, Cell)).value(index))

    def next(self, cell):
        """Tap a list's next value, as its one-place Next button does."""
        return self.press(self.cell(_id(cell, Cell)).next)

    def choose(self, *groups):
        """Choose exactly these venue groups, each held by a group cell of the
        set; none chooses nothing, which means everything."""
        names = [str(group) for group in groups]
        return Live(self._call("perform.choose", {"groups": names}), self)

    def select(self, *patterns):
        """Choose these effects, as Select held while their pads are pressed:
        the next color tapped on a list with no targets is their own color,
        and "below" puts them back on the color below."""
        ids = [self.pattern(pattern).id for pattern in patterns]
        return Live(self._call("perform.select", {"ids": ids}), self)

    def clear(self, effects=False):
        """Choose nothing, as Clear tapped does; effects=True lets every effect
        go instead, as Clear held does."""
        return Live(self._call("perform.clear", {"effects": bool(effects)}), self)

    def base(self, color, on=None, fade=0, unit="beats"):
        """Fade the base color of the lights of groups on (a name or a list,
        each held by a group cell) to color over fade in unit; on=None is
        every light. The base color lights nothing: effects with blend
        "below" take it."""
        if isinstance(on, str):
            on = [on]
        payload = {"color": _color(color, "base color"), "groups": [str(g) for g in on or ()],
                   "fade": float(fade), "unit": _unit(unit)}
        return Live(self._call("perform.base", payload), self)

    def play(self, pattern, level=None):
        """Turn a pattern's pad on, at level (0-1) when given, to hear it,
        where the groups chosen are (its own selection when none is). A
        pattern with no pad already plays while the set is open."""
        pattern = self.pattern(pattern)
        if level is not None:
            self.set(pattern.control("level"), level)
        return self.live() if pattern.pad is None else self.set(pattern.pad, 1.0)

    def stop(self, pattern):
        """Turn a pattern's pad off; it fades by the pad node's release. A
        pattern with no pad has nothing to turn off: set its level to 0."""
        return self.set(self.pattern(pattern).control("pad"), 0.0)

    # -- bindings --------------------------------------------------------

    def _bindings(self, answer):
        labels = {control.name: control.label for control in self.controls()}
        labels.update((control.name, f"{pattern.name} {control.label}")
                      for pattern in self.patterns for control in pattern.controls)
        return tuple(Binding(b["id"], b["control"], labels.get(b["control"], b["control"]),
                             dict(b["address"]), b["mode"], bool(b["invert"]),
                             b.get("controllerId", ""))
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

    def learn(self, control, mode=None, controller=None):
        """Bind the next MIDI or OSC input heard to control: a Control, a
        Pattern's pad, or a control name. Waits up to 20 seconds for someone to
        move a fader or press a pad, so ask them to first. mode defaults to the
        control's own ("fader" for a fader, "momentary" for a pad). It binds on
        controller, or the venue's first, or a new one named after the port
        heard."""
        if mode is None:
            mode = control.mode if isinstance(control, Control) else (
                "momentary" if isinstance(control, Pattern) else "fader")
        if mode not in BINDING_MODES:
            raise PerformRefused(f"binding mode {mode!r}: expected one of {', '.join(BINDING_MODES)}")
        try:
            payload = {"control": self._name(control), "mode": mode}
            if controller is not None:
                payload["controller"] = _id(controller, Controller)
            answer = self._call("perform.learn", payload)
        except LumaHostCallError as error:
            if error.code == "not_heard":
                raise PerformRefused(str(error)) from None
            raise
        return next(b for b in self._bindings(answer) if b.id == answer["binding"]["id"])

    # -- controllers -----------------------------------------------------

    def _controllers(self, answer):
        return tuple(Controller(c["id"], c["name"], c["port"], bool(c["connected"]),
                                copy.deepcopy(c["profile"]))
                     for c in answer["inputs"]["controllers"])

    def _controller(self, answer, key):
        return next(c for c in self._controllers(answer) if c.id == key)

    def controllers(self):
        """This venue's controllers, oldest first."""
        return self._controllers(self._call("perform.bindings", {}))

    def add_controller(self, name, port=""):
        """Add a controller matching MIDI port port (its name exactly; "" for
        one not heard yet) and return it."""
        answer = self._call("perform.add_controller", {"name": str(name), "port": str(port)})
        return self._controller(answer, answer["id"])

    def update_controller(self, controller, *, name=None, port=None):
        """Rename a controller, or match it to another port."""
        key = _id(controller, Controller)
        payload = {"id": key}
        if name is not None:
            payload["name"] = str(name)
        if port is not None:
            payload["port"] = str(port)
        return self._controller(self._call("perform.controller", payload), key)

    def set_profile(self, controller, profile):
        """Give a controller its profile (see Controller). A profile that
        cannot be drawn raises PerformRefused."""
        key = _id(controller, Controller)
        answer = self._call("perform.controller", {"id": key, "profile": _plain(profile)})
        return self._controller(answer, key)

    def remove_controller(self, controller):
        """Remove a controller and its bindings."""
        self._call("perform.remove_controller", {"id": _id(controller, Controller)})

    def light(self, controller, address, color, shine="steady"):
        """Show color ((r, g, b) 0-1 or "#RRGGBB"; black is off) on the control
        at address of a connected controller, shining "steady", "pulse" or
        "blink", through its profile: to check a profile with the person ("is
        the top-left pad red?"). It stays until that control's look changes."""
        if shine not in SHINES:
            raise PerformRefused(f"shine {shine!r}: expected one of {', '.join(SHINES)}")
        self._call("perform.light", {"id": _id(controller, Controller), "address": dict(address),
                                     "color": _color(color, "light"), "shine": shine})

    def map_page(self, controller, page=None):
        """Put a page (the first when None) on a controller's grid by its
        profile: each cell's controls on the pads and faders at its places,
        a pad for a pad, a fader for a fader, in place of what the controller
        had bound there. Returns the controller's bindings."""
        payload = {"id": _id(controller, Controller)}
        if page is not None:
            payload["page"] = self.page(page).id
        answer = self._call("perform.map_page", payload)
        key = payload["id"]
        return tuple(b for b in self._bindings(answer) if b.controller == key)
