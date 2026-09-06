# Python Pattern authoring

Track agents can create score-local Patterns through the existing Python host.
The native GPUI editor opens the resulting ordinary authored graph. No additional
schema, file store, Python evaluator, or migration is introduced.

```python
p = luma.track.pattern("Soft downstage chase")
shape = p.node("soft_edges", softness=0.3)
chase = p.node("chase", shape=shape.output("shape"), mapping="v")
p.expose(chase, "width")
p.expose(chase, "travel")
p.check()
pattern_id = p.save()

edit = luma.track.edit()
edit.add_clip(pattern_id, bars=(1, 5), z=0, selection="front_wash",
              args={"width": 0.4})
edit.check()
edit.window(bars=(1, 5)).output.heatmap()
edit.apply()
```

- `luma.patterns.nodes` contains the engine's actual typed definitions.
- `p.definition(name)` reads a definition, including nested built-in graphs.
  Follow their referenced definition names to inspect subnodes.
- `p.source()` emits replayable Python; `p.graph()` exports the native graph.
- `node.output(port)` connects a typed output to another node's input.
- `p.expose(node, input)` promotes a value to a clip control and preserves its
  default. Selection is attached automatically to the Pattern's Lighting output.
- `p.check()` uses the same graph lowering and type/rate validation as playback.
  It does not render: use the candidate track preview to check venue/audio output.
- `p.save()` saves through authored history and closes the draft. Its id carries
  the schema needed to place a clip immediately, even if the draft was opened in
  an earlier cell. Identical requests retried within the durable turn reuse the
  creation result.

The host captures the score and principal; Python cannot select a different save
scope. This first implementation supports the main editable track thread.
Child workspace creation needs authored Pattern lifecycle/merge support before
it can be enabled without leaking draft work into the main library.

## Remaining work

Custom saved Patterns cannot yet be used as subnodes. The graph file still uses
its existing synthetic `pattern_args` representation for exposed controls; the
Python API hides that wiring, but the native canvas still needs the planned
interface cleanup. Structured Mapping values now pass graph validation, though
the native mapping widget still exposes only its basic choices. Those are the
next consistency gaps, before claiming a complete shared authoring experience.
