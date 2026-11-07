#!/usr/bin/env python3
"""Focused stdlib tests for `luma.perform`: what each verb asks the host,
and what it reads back.

Run directly with either the bundled environment or an ordinary Python::

    python3 backend/python/luma_exec/tests/test_perform.py
"""

from __future__ import annotations

import copy
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Any

PACKAGE_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(PACKAGE_ROOT))

from luma_exec import clip  # noqa: E402
from luma_exec.bindings import build_namespace, reconcile_facades  # noqa: E402
from luma_exec.clip import color, control, curve  # noqa: E402
from luma_exec.host_errors import LumaHostCallError, PerformRefused  # noqa: E402
from luma_exec.perform import Perform  # noqa: E402

WASH = "p-wash"


def graph_json(pad: bool = True) -> dict:
    """A graph with one fader node, `speed`, and a pad, `button1`, on its alpha."""
    nodes = {
        "speed": {"kind": "control", "settings": {"name": "speed"}, "inputs": {}},
        "curve1": {"kind": "curve", "inputs": {"x": {"node": "speed"}}},
        "color1": {"kind": "color", "inputs": {"brightness": {"node": "curve1"}}}}
    if pad:
        nodes["button1"] = {"kind": "button", "settings": {"name": "pad", "mode": "toggle"}}
        nodes["curve2"] = {"kind": "curve", "inputs": {"x": {"node": "button1"}}}
        nodes["color1"]["inputs"]["alpha"] = {"node": "curve2"}
    return {"version": 3, "nodes": nodes}


def described(patterns: list[dict], cells: list[dict]) -> dict:
    """The host's answer: the set and the controls it plays under."""
    def controls(p: dict) -> dict:
        padded = "button1" in p["graph"]["nodes"]
        nodes = [{"node": "speed", "label": "speed", "kind": "fader",
                  "control": f"perform_p{p['id']}_n_speed"}]
        if padded:
            nodes.insert(0, {"node": "button1", "label": "pad", "kind": "button",
                             "control": f"perform_p{p['id']}_n_button1"})
        if "hue" in p["graph"]["nodes"]:
            nodes.append({"node": "hue", "label": "main", "kind": "color",
                          "control": f"perform_p{p['id']}_n_hue"})
        return {"pad": "button1" if padded else None, "level": f"perform_p{p['id']}_level",
                "nodes": nodes}
    return {
        "patterns": patterns,
        "layout": {"pages": [{"id": "page-main", "name": "Main", "columns": 8, "rows": 4,
                              "oneAtATime": [1], "cells": cells}]},
        "controls": {
            "patterns": {p["id"]: controls(p) for p in patterns},
            "cells": {**{c["id"]: f"perform_c{c['id']}" for c in cells
                         if c["content"]["kind"] in ("fader", "group")},
                      **{c["id"]: {"values": [f"perform_c{c['id']}_v{i}"
                                              for i in range(len(c["content"]["values"]))],
                                   "next": f"perform_c{c['id']}"}
                         for c in cells if c["content"]["kind"] == "list"}},
            "master": "perform_master",
            "tap": "perform_tap",
            "select": "perform_select",
            "clear": "perform_clear",
        },
    }


def pattern(id: str, name: str, z: int = 0, pad: bool = True) -> dict:
    return {"id": id, "venueId": "venue-1", "name": name, "selection": {"expression": "front_wash"},
            "zIndex": z, "blendMode": "replace", "seed": 18446744073709551615,
            "graph": graph_json(pad)}


def cell(id: str, at: tuple, content: dict) -> dict:
    return {"id": id, "column": at[0], "row": at[1], "width": 1, "height": 1, "content": content}


def lit(id: str, name: str) -> dict:
    """A pattern whose color is a live color, `hue`, named main."""
    value = pattern(id, name)
    nodes = value["graph"]["nodes"]
    nodes["hue"] = {"kind": "control", "settings": {"name": "main", "kind": "color"}}
    nodes["color1"]["inputs"]["color"] = {"node": "hue"}
    return value


COLORS = {"kind": "list", "label": "Colors", "values": [{"color": [1, 0, 0]}, {"color": [0, 0, 1]}], "fade": 2.0,
          "unit": "beats", "targets": [{"kind": "node", "patternId": WASH, "node": "hue"}],
          "step": {"bars": 4, "loops": True}}
LIT = described([lit(WASH, "Warm wash")],
                [cell("cell-wash", (0, 0), {"kind": "pattern", "patternId": WASH}),
                 cell("cell-colors", (1, 0), COLORS)])



START = described([pattern(WASH, "Warm wash")],
                  [cell("cell-wash", (0, 0), {"kind": "pattern", "patternId": WASH}),
                   cell("cell-tap", (7, 0), {"kind": "tap"})])


class Host:
    """Records every call, and answers what the test says it should."""

    def __init__(self) -> None:
        self.calls: list[tuple[str, Any]] = []
        self.answers: dict[str, Any] = {}

    def __call__(self, method: str, payload: Any) -> Any:
        self.calls.append((method, copy.deepcopy(payload)))
        answer = self.answers[method]
        if isinstance(answer, Exception):
            raise answer
        return copy.deepcopy(answer)


def namespace(host: Host, venue: str = "venue-1", revision: str = "r-1", set: dict = START,
              presets: dict | None = None):
    return build_namespace({
        "schema_version": 1, "revision": revision, "agent_kind": "perform_set",
        "scope": {"venue_id": venue},
        "root": {"perform": {"venue_id": venue, "set": set}, "presets": presets or {}},
    }, Path(tempfile.mkdtemp(prefix="luma-perform-")), host_call=host)


class ReadingTests(unittest.TestCase):
    def test_the_namespace_installs_the_facade_over_the_set(self) -> None:
        perform = namespace(Host()).perform
        self.assertIsInstance(perform, Perform)
        [wash] = perform.patterns
        self.assertEqual((wash.name, wash.selection, wash.seed), ("Warm wash", "front_wash", 2**64 - 1))
        self.assertEqual(wash.pad.name, f"perform_p{WASH}_n_button1", "its pad node's control")
        self.assertEqual((wash.pad.kind, wash.pad.mode), ("button", "momentary"))
        self.assertIs(wash.control("pad"), wash.pad)
        self.assertEqual(wash.cell.at, (0, 0))
        self.assertEqual(wash.control("speed").name, f"perform_p{WASH}_n_speed")
        self.assertEqual(wash.control("level").mode, "fader")
        self.assertEqual(perform.page().one_at_a_time, (1,))
        self.assertIs(perform.pattern("Warm wash").id, perform.pattern(WASH).id)
        text = perform.describe()
        self.assertIn("'Warm wash' on front_wash", text)
        self.assertIn("(7, 0): tap", text)
        with self.assertRaises(KeyError):
            perform.pattern("Cold wash")

    def test_a_pattern_without_a_pad_says_so(self) -> None:
        set = described([pattern("p-bare", "Bare", pad=False)],
                         [cell("cell-bare", (0, 0), {"kind": "pattern", "patternId": "p-bare"})])
        perform = namespace(Host(), set=set).perform
        [bare] = perform.patterns
        self.assertIsNone(bare.pad)
        with self.assertRaises(KeyError) as raised:
            bare.control("pad")
        self.assertIn("no pad", str(raised.exception))
        self.assertIn("no pad", perform.describe())

    def test_live_names_the_patterns_whose_pads_are_on(self) -> None:
        host = Host()
        set = described([pattern(WASH, "Warm wash"), pattern("p-cold", "Cold wash"),
                         pattern("p-bare", "Bare", pad=False)], [])
        host.answers["perform.live"] = {"open": True, "state": {
            "controls": {f"perform_p{WASH}_level": 0.5, "perform_master": 0.8},
            "buttons": {
                f"perform_p{WASH}_n_button1": {"press": {}, "gate": {"onFor": 2.0, "offFor": None}},
                "perform_pp-cold_n_button1": {"press": {}, "gate": {"onFor": 2.0, "offFor": 1.0}},
            },
            "clock": {"kind": "tap", "bpm": 124.0, "beat": 1.0}, "decks": []}}
        live = namespace(host, set=set).perform.live()
        [(wash, level), (bare, _)] = live.playing
        self.assertEqual((wash.name, level, live.master), ("Warm wash", 0.5, 0.8))
        self.assertEqual(bare.name, "Bare", "with no pad it plays while the set is open")
        self.assertIn("124.0 BPM", repr(live))
        host.answers["perform.live"] = {"open": False}
        self.assertIn("not open", repr(namespace(host).perform.live()))

    def test_live_names_the_presses_waiting_for_their_beat(self) -> None:
        host = Host()
        set = described([pattern(WASH, "Warm wash"), pattern("p-cold", "Cold wash")], [])
        pad = f"perform_p{WASH}_n_button1"
        host.answers["perform.live"] = {"open": True, "state": {
            "controls": {},
            "buttons": {
                pad: {"press": {"start": "bar"}, "gate": {"onFor": -0.5, "offFor": None}},
                "perform_pp-cold_n_button1": {"press": {"start": "beat"},
                                              "gate": {"onFor": 3.0, "offFor": -0.2}},
            },
            "effects": {WASH: {"groups": ["front"], "below": True}},
            "decks": []}}
        live = namespace(host, set=set).perform.live()
        self.assertEqual([(p.name, going) for p, going in live.waiting],
                         [("Warm wash", "on"), ("Cold wash", "off")])
        self.assertEqual([p.name for p, _ in live.playing], ["Cold wash"],
                         "on until its beat; the other not yet")
        self.assertEqual([(p.name, groups, below) for p, groups, below in live.effects],
                         [("Warm wash", ("front",), True)])
        self.assertIn("'Warm wash' to go on", repr(live))
        self.assertIn("'Cold wash' to go off", repr(live))


class ChangeTests(unittest.TestCase):
    def test_add_sends_the_graph_and_reads_back_what_the_host_made(self) -> None:
        host = Host()
        perform = namespace(host).perform
        made = described([pattern("p-new", "Pulse", 1), pattern(WASH, "Warm wash")],
                         START["layout"]["pages"][0]["cells"]
                         + [cell("cell-new", (2, 1), {"kind": "pattern", "patternId": "p-new"})])
        host.answers["perform.add"] = {"id": "p-new", "cell": "cell-new", "set": made}
        graph = color(brightness=curve(control("speed"), [[0, 0], [1, 1]]))
        added = perform.add(graph, name="Pulse", selection="front_wash", at=(2, 1))
        method, payload = host.calls[-1]
        self.assertEqual(method, "perform.add")
        self.assertEqual(payload["at"], [2, 1])
        self.assertEqual(payload["blend"], "replace")
        self.assertEqual(payload["graph"], graph.json())
        self.assertNotIn("pad", payload, "the host gives a graph with none its pad")
        self.assertNotIn("z", payload, "the host puts it on top")
        self.assertEqual((added.name, added.cell.at), ("Pulse", (2, 1)))
        self.assertEqual([p.name for p in perform.patterns], ["Pulse", "Warm wash"])

    def test_busk_asks_the_host_for_a_page_and_reads_it_back(self) -> None:
        host = Host()
        perform = namespace(host).perform
        made = copy.deepcopy(START)
        made["layout"]["pages"].append({"id": "page-busk", "name": "Busk", "columns": 7, "rows": 5,
                                        "oneAtATime": [], "cells": []})
        host.answers["perform.busk"] = {"id": "page-busk", "set": made}
        page = perform.busk()
        self.assertEqual(host.calls[-1], ("perform.busk", {}))
        self.assertEqual((page.id, page.name), ("page-busk", "Busk"))
        self.assertEqual([p.name for p in perform.pages], ["Main", "Busk"])

    def test_a_preset_names_the_pattern_and_gives_its_blend(self) -> None:
        presets = {"clips": [{"name": "Fan", "blend_mode": "offset", "graph": graph_json()}]}
        host = Host()
        host.answers["perform.add"] = {"id": WASH, "cell": "cell-wash", "set": START}
        namespace(host, presets=presets).perform.add("Fan")
        payload = host.calls[-1][1]
        self.assertEqual((payload["name"], payload["blend"], payload["selection"]), ("Fan", "offset", "all"))

    def test_a_refusal_is_perform_refused(self) -> None:
        host = Host()
        host.answers["perform.update"] = LumaHostCallError("refused", "cells a and b overlap")
        perform = namespace(host).perform
        with self.assertRaises(PerformRefused) as raised:
            perform.update(WASH, name="Cold wash")
        self.assertIn("overlap", str(raised.exception))
        self.assertEqual(perform.pattern(WASH).name, "Warm wash", "nothing changed")
        with self.assertRaises(PerformRefused):
            perform.add(clip.Graph(None), at=(-1, 0), name="x")

    def test_fader_targets_are_levels_fader_nodes_and_venue_controls(self) -> None:
        host = Host()
        host.answers["perform.place"] = {"id": "cell-tap", "set": START}
        perform = namespace(host).perform
        wash = perform.pattern(WASH)
        perform.place("fader", label="Speeds", targets=[wash, wash.control("speed"), "drop"])
        self.assertEqual(host.calls[-1][1]["content"]["targets"], [
            {"kind": "level", "patternId": WASH},
            {"kind": "node", "patternId": WASH, "node": "speed"},
            {"kind": "named", "name": "drop"}])
        with self.assertRaises(PerformRefused):
            perform.place("fader", targets=[wash.control("pad")])
        with self.assertRaises(PerformRefused):
            perform.place("pattern")


FRONT = {"kind": "group", "label": "", "group": "front"}
AIMED = described([pattern(WASH, "Warm wash")],
                  [cell("cell-wash", (0, 0), {"kind": "pattern", "patternId": WASH}),
                   cell("cell-front", (1, 0), FRONT)])


class GroupTests(unittest.TestCase):
    def test_a_group_cell_reads_back_its_group_and_is_pressed_by_its_control(self) -> None:
        perform = namespace(Host(), set=AIMED).perform
        front = perform.cell("cell-front")
        self.assertEqual((front.kind, front.group), ("group", "front"))
        [pressed] = [c for c in perform.controls() if c.kind == "group"]
        self.assertEqual((pressed.name, pressed.mode), ("perform_ccell-front", "momentary"))
        self.assertIn("(1, 0): group front (cell-front)", perform.describe())
        kinds = {c.kind: c.name for c in perform.controls()}
        self.assertEqual((kinds["select"], kinds["clear"]), ("perform_select", "perform_clear"))

    def test_place_and_update_a_group_cell_and_the_utilities(self) -> None:
        host = Host()
        host.answers["perform.place"] = {"id": "cell-front", "set": AIMED}
        host.answers["perform.cell"] = {"id": "cell-front", "set": AIMED}
        perform = namespace(host).perform
        perform.place("group", group="front", at=(1, 0))
        self.assertEqual(host.calls[-1][1]["content"], FRONT)
        perform.update_group("cell-front", group="back")
        self.assertEqual(host.calls[-1][1], {"id": "cell-front", "group": "back"})
        for kind in ("select", "clear"):
            perform.place(kind)
            self.assertEqual(host.calls[-1][1]["content"], {"kind": kind})
        with self.assertRaises(PerformRefused):
            perform.place("group")
        with self.assertRaises(PerformRefused):
            perform.place("target", group="front")

    def test_choose_play_select_clear_and_base_ask_the_host(self) -> None:
        host = Host()
        pad = f"perform_p{WASH}_n_button1"
        state = {"open": True, "state": {
            "controls": {}, "decks": [], "chosen": ["front"], "selected": [WASH], "selecting": True,
            "buttons": {pad: {"press": {}, "gate": {"onFor": 1.0, "offFor": None}}},
            "effects": {WASH: {"groups": ["front"], "below": False}},
            "base": {"front": [1.0, 0.0, 0.0]}}}
        for method in ("perform.choose", "perform.control", "perform.select", "perform.clear",
                       "perform.base"):
            host.answers[method] = state
        perform = namespace(host, set=AIMED).perform
        live = perform.choose("front")
        self.assertEqual(host.calls[-1], ("perform.choose", {"groups": ["front"]}))
        self.assertEqual(live.chosen, ("front",))
        live = perform.play(WASH)
        self.assertEqual(host.calls[-1], ("perform.control", {"name": pad, "value": 1.0}))
        self.assertIn("'Warm wash' on front at 1.00", repr(live))
        perform.select(WASH)
        self.assertEqual(host.calls[-1], ("perform.select", {"ids": [WASH]}))
        self.assertEqual([p.name for p in live.selected], ["Warm wash"])
        perform.clear()
        self.assertEqual(host.calls[-1], ("perform.clear", {"effects": False}))
        perform.clear(effects=True)
        self.assertEqual(host.calls[-1], ("perform.clear", {"effects": True}))
        live = perform.base((1, 0, 0), on="front", fade=1)
        self.assertEqual(host.calls[-1], ("perform.base", {
            "color": [1.0, 0.0, 0.0], "groups": ["front"], "fade": 1.0, "unit": "beats"}))
        self.assertEqual(live.base, {"front": (1.0, 0.0, 0.0)})
        with self.assertRaises(TypeError):
            perform.play(WASH, on=["front"])


class ListTests(unittest.TestCase):
    def test_a_list_reads_back_its_values_and_their_controls(self) -> None:
        perform = namespace(Host(), set=LIT).perform
        colors = perform.cell("cell-colors")
        self.assertEqual((colors.kind, colors.values, colors.step), ("list", ((1, 0, 0), (0, 0, 1)), (4, True)))
        self.assertEqual((colors.value(1).name, colors.value(1).mode), ("perform_ccell-colors_v1", "momentary"))
        self.assertEqual(colors.next.name, "perform_ccell-colors")
        self.assertEqual(perform.pattern(WASH).control("main").kind, "color")
        self.assertIn(colors.value(0), perform.controls())
        self.assertIn(colors.next, perform.controls())
        self.assertIn("list 'Colors' of 2 colors setting 1 targets, fading over 2.0 beats, the next every 4 "
                      "bars, looping", perform.describe())
        with self.assertRaises(KeyError):
            colors.value(2)

    def test_place_a_color_list_sets_live_colors_and_a_number_list_what_faders_move(self) -> None:
        host = Host()
        host.answers["perform.place"] = {"id": "cell-colors", "set": LIT}
        perform = namespace(host, set=LIT).perform
        wash = perform.pattern(WASH)
        perform.place("list", values=[(1, 0, 0), "#0000ff"], targets=[wash.control("main")],
                      fade=2, step=(4, True))
        content = host.calls[-1][1]["content"]
        self.assertEqual(content["values"][0], {"color": [1, 0, 0]})
        self.assertEqual(len(content["values"][1]["color"]), 3, "hex reads as a color")
        self.assertEqual((content["label"], content["fade"], content["unit"], content["step"]),
                         ("Colors", 2.0, "beats", {"bars": 4, "loops": True}))
        self.assertEqual(content["targets"], COLORS["targets"])
        perform.place("list", values=[(1, 0, 0), "below"])
        self.assertEqual(host.calls[-1][1]["content"]["values"], [{"color": [1, 0, 0]}, "below"])
        perform.place("list", values=[0, 0.5, 1], targets=[wash.control("speed")])
        content = host.calls[-1][1]["content"]
        self.assertEqual(content["values"], [{"number": 0.0}, {"number": 0.5}, {"number": 1.0}])
        self.assertEqual(content["targets"], [{"kind": "node", "patternId": WASH, "node": "speed"}])
        for wrong in (wash.control("speed"), wash, "drop"):
            with self.subTest(wrong), self.assertRaises(PerformRefused):
                perform.place("list", values=[(1, 0, 0)], targets=[wrong])
        with self.assertRaises(PerformRefused):
            perform.place("list", values=[0.5], targets=[wash.control("main")])
        with self.assertRaises(PerformRefused):
            perform.place("fader", targets=[wash.control("main")])
        with self.assertRaises(PerformRefused):
            perform.place("list", values=[(1, 0)])
        with self.assertRaises(PerformRefused):
            perform.place("list", values=[(1, 0, 0)], step=(0, True))

    def test_update_list_sends_what_is_given_and_none_stops_stepping(self) -> None:
        host = Host()
        host.answers["perform.cell"] = {"id": "cell-colors", "set": LIT}
        perform = namespace(host, set=LIT).perform
        perform.update_list("cell-colors", values=[(0, 1, 0)], unit="seconds")
        self.assertEqual(host.calls[-1][1], {"id": "cell-colors", "values": [{"color": [0, 1, 0]}],
                                             "unit": "seconds"})
        perform.update_list("cell-colors", step=None)
        self.assertEqual(host.calls[-1][1], {"id": "cell-colors", "step": None})
        perform.update_list("cell-colors", targets=[perform.pattern(WASH).control("main")])
        self.assertEqual(host.calls[-1][1]["targets"], COLORS["targets"])

    def test_tap_and_next_press_the_list_and_live_reads_the_colors(self) -> None:
        host = Host()
        live = {"open": True, "state": {
            "controls": {}, "buttons": {}, "clock": None, "decks": [],
            "colors": {f"perform_p{WASH}_n_hue": {
                "fade": {"from": [1, 1, 1], "to": [0, 0, 1], "length": 2.0, "unit": "beats"}, "since": 0.5}},
            "lists": {"cell-colors": 1}}}
        host.answers["perform.press"] = live
        perform = namespace(host, set=LIT).perform
        tapped = perform.tap(perform.cell("cell-colors"), 1)
        self.assertEqual([call for call in host.calls], [
            ("perform.press", {"name": "perform_ccell-colors_v1", "down": True}),
            ("perform.press", {"name": "perform_ccell-colors_v1", "down": False})])
        self.assertEqual(tapped.colors[f"perform_p{WASH}_n_hue"], (0, 0, 1))
        self.assertEqual(tapped.lists, {"cell-colors": 1})
        self.assertIn("cell cell-colors value 1", repr(tapped))
        perform.next("cell-colors")
        self.assertEqual(host.calls[-1], ("perform.press", {"name": "perform_ccell-colors", "down": False}))


class BindingTests(unittest.TestCase):
    def test_learn_binds_a_pad_as_a_pad_and_names_what_it_bound(self) -> None:
        host = Host()
        binding = {"id": "b1", "control": f"perform_p{WASH}_n_button1", "mode": "momentary",
                   "invert": False, "address": {"kind": "midi_note", "channel": 0, "number": 36}}
        host.answers["perform.learn"] = {"binding": binding, "inputs": {"bindings": [binding]}}
        perform = namespace(host).perform
        learned = perform.learn(perform.pattern(WASH))
        self.assertEqual(host.calls[-1][1], {"control": f"perform_p{WASH}_n_button1", "mode": "momentary"})
        self.assertEqual(learned.label, "Warm wash pad")
        self.assertIn("MIDI note 36 ch 1", repr(learned))
        perform.learn(perform.pattern(WASH).control("speed"))
        self.assertEqual(host.calls[-1][1]["mode"], "fader")
        host.answers["perform.learn"] = LumaHostCallError("not_heard", "Nothing was heard.")
        with self.assertRaises(PerformRefused):
            perform.learn("drop")


class RefreshTests(unittest.TestCase):
    def test_a_new_cell_keeps_the_facade_and_another_venue_revokes_it(self) -> None:
        host = Host()
        first = reconcile_facades(namespace(host, revision="a"), None)
        perform = first.perform
        later = described([], [])
        second = reconcile_facades(namespace(host, revision="b", set=later), first)
        self.assertIs(second.perform, perform)
        self.assertEqual(perform.patterns, ())
        third = reconcile_facades(namespace(host, venue="venue-2", revision="c"), second)
        self.assertIsNot(third.perform, perform)
        with self.assertRaises(RuntimeError):
            perform.remove(WASH)


if __name__ == "__main__":
    unittest.main(verbosity=2)
