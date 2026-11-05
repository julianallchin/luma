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
        return {"pad": "button1" if padded else None, "level": f"perform_p{p['id']}_level",
                "nodes": nodes}
    return {
        "patterns": patterns,
        "layout": {"pages": [{"id": "page-main", "name": "Main", "columns": 8, "rows": 4,
                              "oneAtATime": [1], "cells": cells}]},
        "controls": {
            "patterns": {p["id"]: controls(p) for p in patterns},
            "cells": {c["id"]: f"perform_c{c['id']}" for c in cells
                      if c["content"]["kind"] == "fader"},
            "master": "perform_master",
            "tap": "perform_tap",
        },
    }


def pattern(id: str, name: str, z: int = 0, pad: bool = True) -> dict:
    return {"id": id, "venueId": "venue-1", "name": name, "selection": {"expression": "front_wash"},
            "zIndex": z, "blendMode": "replace", "seed": 18446744073709551615,
            "graph": graph_json(pad)}


def cell(id: str, at: tuple, content: dict) -> dict:
    return {"id": id, "column": at[0], "row": at[1], "width": 1, "height": 1, "content": content}


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
