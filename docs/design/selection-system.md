# Selection System

The selection system turns a selection into a list of fixture heads. A
selection names groups of the score's venue, never fixtures.

## Selections

File: `backend/crates/patterns/src/selection.rs`

A selection is the whole venue or the union of some of its groups. Each group
is named by the id of its row:

```json
{ "all": true }
{ "groups": ["0b0f…", "9c1e…"] }
```

A clip, a Perform pattern and a Perform group cell reference groups this way.
A reference is to the group, not to its name, so renaming a group changes no
selection. A group that has been deleted is still listed and selects nothing.

A selection always means every head of the groups it lists. For a random share
of heads, use a `shuffle` node read by an order `space` and a curve, as the
Sparkle preset does. For any other set, such as the fixtures two groups share
or a few heads of a fixture, make a group that holds it.

### Resolution

File: `backend/src/services/groups.rs`, function `resolve_selection`.

`resolve_selection` returns the matched fixtures in venue fixture order, each
with the heads it matched on. A group holds a whole fixture or some of its
heads. A fixture matched on every head is returned whole.

### Names

People and the agent write groups by name: `all`, one group name, or several
joined with `|`.

```
all                          -- every fixture in the venue
front_wash                   -- fixtures in the front_wash group
left_truss | right_truss     -- fixtures in either group
```

`GroupNames` in `backend/src/services/groups.rs` translates between the two
forms where a name is typed or shown. `parse` turns the text into ids and
refuses a name the venue has no group for. `spell` writes a selection by name.
It leaves out a deleted group, and writes `none` when no group is left. The
names are never stored.

## Cells

A graph does not reference fixtures. The host resolves the selection of each
clip and gives the engine a list of `Cell` values
(`backend/crates/patterns/src/mapping.rs`):

| Field | Meaning |
|-------|---------|
| `id` | Head identity |
| `group` | The selection the head came from: `all`, or its group ids joined by `\|` |
| `world` | Position in venue space: X stage right, Y upstage, Z up |
| `uvz` | Position in stage space |

`Cell::stage_coordinates` converts venue space to stage space: U right, V
downstage, Z up.

## Positions in a clip graph

A `space` node turns the cells into a coordinate per head. Its `kind` is
`line` (along a direction), `order` (sorted along a direction), `radial`
(distance from a centre) or `angle` (turns around a centre). Its ruler runs
0–1 across the clip's selection. A `split` node makes each fixture or each
group its own span, and a `mirror` node folds the heads across a plane. A
negative direction runs the other way. See the
[node reference](https://luma.show/docs/node-reference/coordinates#space).

## Rig UV

File: `backend/src/rig_uv.rs`, function `rig_uv`.

`rig_uv` gives each position a `(u, v)` pair from 0 to 1 over the selection:

- **`u`** is the first principal component of the horizontal (XY) spread. Its
  sign is set so that its largest world component is positive. With no
  horizontal spread, `u` is the normalized index.
- **`v`** is world height (Z) over the selection. With no height spread, `v`
  is 0.5.

The agent venue tools use `rig_uv`
(`backend/src/agent_execution/bindings/providers/venue.rs`).
