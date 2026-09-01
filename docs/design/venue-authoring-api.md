# Venue Authoring API v2 — cursors, vectors, queries, drafts

Status: settled spec (2026-08-31), co-designed over ~12 review rounds. This is the
contract for the agent-facing build surface in `luma_exec/venue.py` and the host
verbs behind it. It replaces socket-name-driven authoring (`extend(node,
"corner_fl", 4.0)`) for agents; the old verbs remain as the low-level layer the
new surface compiles onto.

## Why (the failure this kills)

The three demo-venue builds failed in predictable ways: agents did blind
arithmetic against a point-based API, socket names (`end_b`, `face_-y`,
`corner_fl`) forced them to memorize per-piece vocabulary, and the feedback
instruments (`describe()` prints unlabeled data-space mesh-origin poses) could
not catch a rig built one module off-centre. The fix is an API where intent is
stated in the venue frame with vectors, joints are explicit chain elements,
every write returns ground truth, and the read side is an equal partner.

## Frame contract (printed in the binding header, verbatim)

- `+u` = stage right, `+v` = toward the crowd, `+z` = up. Upstage is `-v`.
- Angles in degrees. Lengths in metres.
- 0.5 m structural module. Speakers / CDJs / equipment are exempt — they are
  endpoints; nothing chains off them.

Five contract sentences (each is a rule the implementation enforces):

1. `at=` is the footprint **centre** in plan, everywhere. A free `place`
   anchors by centre; `add` grows from a tip — same piece, two verbs, and the
   docstrings say so explicitly.
2. `v.tip(node, end=vector)` grabs a cursor on any **existing** node's free
   end. `end` is a direction vector, not a socket name.
3. Hinge sign: right-hand rule — positive = counterclockwise about `+axis`;
   angle ∈ [−90, +90], 5° steps, 0 = straight coupler. `axis` must be ⊥ the
   incoming run; a bad axis is refused naming the valid plane.
4. `direction=` (an exit direction) and `axis=` (a rotation axis) are distinct
   parameter names; a wrong-kind error says which was expected.
5. 1-D `at=` on stick hosts is signed metres from **midspan** (`at=0` =
   centre).

**No socket names in this API. Ever.** No `end_b`, no `face_-x`, no
`corner_fl`. Vectors are how you name faces and ends. No shape verbs either —
box / circle / spiral are the agent's own Python loops over these primitives.

## Cursor chain grammar

```python
t = v.place("truss", at=(-5.5, 5), length=8, direction=(0, 0, 1))   # tower up
t = t.add("corner")
t = t.add("truss", length=11, direction=(1, 0, 0))   # direction NAMES the corner's exit face
t = t.add("corner")
t = t.add("truss", length=8, direction=(0, 0, -1))

# hinge (guardrails, angled runs):
g = v.place("guardrail", at=(0, 8))
g = g.add("hinge", axis=(0, 0, 1), angle=30)
g = g.add("guardrail")                # continues along the hinge's out — needs nothing
```

- Every `place`/`add` returns a **tip cursor**; the cursor is also a node
  handle (`.id`, `.at`, `.node` …) so it feeds every other verb.
- Joints are **explicit chain elements** (user's decision): `add("corner")`,
  `add("hinge", axis=, angle=)`. A corner's exit face is chosen by the *next*
  `add`'s `direction=` (quantized to the corner's legal 90° set). A hinge sets
  the out-direction itself, so the following `add` needs no direction.
- `direction` on a chained stick may be omitted when the joint already
  determines it; giving one that contradicts the joint is refused.
- Chained sticks default their length; `length=` overrides in module steps.

## Error behaviour (the contract's teeth)

- **Off-module** → snap + announce: build the nearest legal thing and say so —
  `"built 7.0 m, landed at (4.95, 4.95, 0)"`. The vector is intent, the graph
  is truth, and the **return value is always the actual tip**, so drift never
  accumulates silently.
- **Impossible turn** → refuse, listing the legal alternatives *as vectors*.
- **Collision** (including a chain extending back through itself) → refuse,
  citing the blocking node by id + label.
- **`to=node`** form for exact meets: `t.add("truss", to=other_tower)` lets the
  solver spend length steps and hinge steps together to land the connection.

## Hosting & lights

`on=` reframes `at=` into the host's own frame. Deck host → 2-D `at=(u, v)` on
its top. Stick host → 1-D signed metres from midspan.

`face=` is a **vector** mapped to the piece's nearest mounting face. Beam =
mount normal, so choosing the face is choosing where the light points at rest.

```python
v.place("dbr15", on=stage, at=(4, -1))
v.distribute("ledbeam", on=beam, count=8, face=(0, 1, 0))    # crowd face
v.distribute("spiider", on=beam, count=6, span=(-4, 4))       # signed metres from midspan
v.place("pointe", on=beam, at=2.0, face=(0, 0, -1))
v.aim(heads, direction=(0, 1, -0.5))       # or aim(heads, at=(0, 8, 0))
```

- `distribute` overcrowding refuses naming the max count (the catalog knows
  fixture size).
- Aiming is separate from mounting: `aim(selection, direction=vec)` or
  `aim(selection, at=point)`.
- **`v.toward(point_or_node)`** is accepted anywhere a direction vector is;
  resolved per host (two flanking trusses each get their own inward vector from
  one stated intent). The graph stores only the resolved vector.

## Query API (equal partner to the write side)

```python
v.nodes(kind=, label=glob, on=, region=)   # -> objects
#   .id .label .kind .at (facade centre) .size .host .tips .face
v.extent(selection)   # -> span + centre in u/v — the one-line "is it centred" check
```

Every field a query returns is legal input to the write verbs, so
read → edit → verify round-trips. `describe()` / `tiles()` demote to
human-readable views over this data. 2-D deck spans stay:
`v.place("deck", span=(10, 3), at=(0, -1.5))` is one node.

`catalog["guardrail"].size` — machine-readable dims per piece; short names
(`"truss"`, `"guardrail"`, `"deck"`) are the primary ids, glb paths demoted to
aliases.

## Drafts (build → preview → stamp)

A component is a function over the same verbs, run somewhere that isn't the
venue yet:

```python
def portal(s, width=11, height=8):
    t = s.place("truss", at=(-width/2, 0), length=height, direction=(0, 0, 1))
    t = t.add("corner")
    beam = t = t.add("truss", length=width, direction=(1, 0, 0))
    t = t.add("corner")
    t.add("truss", length=height, direction=(0, 0, -1))
    s.distribute("ledbeam", on=beam, count=8, face=(0, 1, 0))

gate = v.draft(portal, width=11)   # runs against a scratch graph — venue untouched
gate.render()                      # visual preview: isolated, no floor grid clutter
gate.extent                       # textual preview
for i in range(7):
    v.stamp(gate, at=(0, 5 + 6 * i))
```

- `draft(fn, **params)` executes the function against a detached graph. Same
  verbs inside — anything buildable in a venue is draftable.
- Preview both ways: `gate.render()` (isolated render) and
  `gate.extent` / `gate.describe()`.
- `stamp(draft, at=, yaw=)` copies the draft subtree into the venue —
  `duplicate` pointed at a draft. Stamps are plain graph rows; the *function*
  is the source of truth.
- Convergence note: `Geometry::Assembly` (DJ booth) is the static catalog
  version of this. A draft is an agent-authored assembly; promoting one to the
  palette is a future seam, not in scope.

## Layering ruling (Ousterhout: pull complexity downward)

The vector→face mapping, module quantization, snap-and-announce, corner exit
resolution, hinge validation, collision checks, `nodes`/`extent`, and
draft/stamp all live **host-side (Rust)**, next to the solver that owns pose
truth. Python is a thin cursor/facade: it shapes calls and wraps reports. Do
not re-derive geometry in Python — `luma_exec` never sees a mesh.

Suggested seams (implementer may refine, not thin out):

- `gpui/crates/scene/src/venue.rs` + `snap.rs` + `sockets.rs` — where
  vector→socket resolution and chain compilation belong (compile a chain op to
  the existing attach/extend/params graph edits; the graph schema does not
  change).
- `src-tauri/src/agent_execution/venue_host.rs` — new host verbs
  (`venue.chain`, `venue.query`, `venue.extent`, `venue.draft.*`,
  `venue.stamp`), same request/response style as the existing ones.
- `src-tauri/python/luma_exec/venue.py` — cursor class, `draft`, `toward`,
  reworked docstrings; the binding header gains the frame-contract lines.

## Out of scope

- React stage editing (frozen), gpui editor UI changes beyond what reuses the
  new host verbs, promoting drafts to the palette, aim cue layer.
