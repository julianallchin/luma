"""Local builder invariants; real catalogue/compilation/persistence are cell tests."""
import sys
import unittest
from pathlib import Path
from types import SimpleNamespace

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
from luma_exec.patterns import PatternDraft


def port(kind, default=None, rate="frame"):
    return {"name": kind, "description": "", "value_type": kind, "rate": rate,
            "default": None if default is None else {"type": kind, "value": default}}


DEFINITIONS = {
    "shape": {"inputs": {"softness": port("proportion", .2)},
              "outputs": {"shape": port("envelope")}},
    "chase": {"inputs": {"shape": port("envelope"), "width": port("proportion", .25),
                         "color": port("color", [1, 1, 1])},
              "outputs": {"lighting": port("lighting")}},
}


class PatternTests(unittest.TestCase):
    def draft(self, name="Test", **kwargs):
        return PatternDraft(name, DEFINITIONS, self.host, self.registered.append, **kwargs)

    def setUp(self):
        self.calls, self.registered = [], []
        def host(method, payload):
            self.calls.append((method, payload))
            return {"id": "saved", "name": payload["name"], "args": payload["graph"]["args"]}
        self.host = host

    def test_source_roundtrip_preserves_connections_literals_and_controls(self):
        p = self.draft("Don't lose quotes")
        shape = p.node("shape", id="p", softness=.4)
        chase = p.node("chase", id="class", shape=shape.output("shape"), color=[.1, .2, .3])
        p.expose(chase, "width", name="Pill width")
        scope = {"luma": SimpleNamespace(track=SimpleNamespace(pattern=self.draft))}
        exec(p.source(), scope)
        self.assertEqual(p.graph(), scope["p"].graph())
        self.assertEqual(self.calls, [])

    def test_save_is_explicit_and_registers_exactly_once(self):
        p = self.draft()
        p.node("chase")
        self.assertEqual(self.calls, [])
        self.assertEqual(p.save(), "saved")
        self.assertEqual(p.save(), "saved")
        self.assertEqual(len(self.calls), 1)
        self.assertEqual(len(self.registered), 1)
        with self.assertRaisesRegex(ValueError, "already saved"):
            p.node("shape")

    def test_bad_connections_and_duplicate_exposures_are_rejected(self):
        p = self.draft()
        shape = p.node("shape")
        with self.assertRaisesRegex(ValueError, "requires proportion"):
            p.node("chase", width=shape.output("shape"))
        other = self.draft().node("shape")
        with self.assertRaisesRegex(ValueError, "different drafts"):
            p.node("chase", shape=other.output("shape"))
        chase = p.node("chase", shape=shape.output("shape"))
        with self.assertRaisesRegex(ValueError, "Connected inputs"):
            p.expose(chase, "shape")
        p.expose(chase, "width")
        with self.assertRaisesRegex(ValueError, "already exposed"):
            p.expose(chase, "width", id="other_width")

    def test_graph_export_cannot_mutate_draft_or_catalogue(self):
        p = self.draft()
        chase = p.node("chase", color=[.1, .2, .3])
        p.expose(chase, "color")
        graph = p.graph()
        graph["args"][0]["defaultValue"]["r"] = 200
        definition = p.definition("chase")
        definition["inputs"]["color"]["default"]["value"][0] = 0
        self.assertEqual(p.graph()["args"][0]["defaultValue"]["r"], 25.5)
        self.assertEqual(p.definition("chase")["inputs"]["color"]["default"]["value"], [1, 1, 1])

    def test_disconnected_lighting_outputs_need_explicit_composition(self):
        p = self.draft()
        p.node("chase")
        p.node("chase")
        with self.assertRaisesRegex(ValueError, "one Lighting output"):
            p.check()
        self.assertEqual(self.calls, [])


if __name__ == "__main__":
    unittest.main()
