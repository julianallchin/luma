# Selection Node Debugging Session

## Problem Statement
The selection node is "constantly selecting all fixtures" even though the user has configured groups.

## Current Status
**Compile error fixed** - Removed `log::debug!` statement that was causing build failure.
**Debug logging added** - Added `println!` (debug-only) in `selection.rs` to log raw/parsed query and selected fixture count.

---

## Investigation Summary

### 1. Data Flow Analysis

The selection query flows through these stages:

```
Frontend (SelectionQueryBuilder)
    ↓ onChange callback
StandardNode (setParam with JSON.stringify)
    ↓ stores in graph store
Graph Store (useGraphStore)
    ↓ serialize() reads from store
Pattern Editor (serializeGraph)
    ↓ invoke("run_graph") OR invoke("save_pattern_graph")
Backend (run_graph_internal)
    ↓ parses node.params.get("selection_query")
selection.rs (resolve_selection_query_with_path)
```

### 2. Key Files Examined

| File | Purpose | Status |
|------|---------|--------|
| `src-tauri/src/node_graph/nodes/selection.rs` | Parses and executes selection query | Fixed JSON parsing |
| `src/features/universe/components/selection-query-builder.tsx` | UI for configuring query | Looks correct |
| `src/shared/lib/react-flow/standard-node.tsx` | Handles param updates | Looks correct |
| `src/features/patterns/stores/use-graph-store.ts` | Stores node params | Looks correct |
| `src/shared/lib/react-flow-editor.tsx` | Serializes graph for execution | **Potential issue area** |
| `src-tauri/src/compositor.rs` | Runs patterns from database | Uses stored graph JSON |

### 3. Root Cause Hypotheses

#### Hypothesis A: Graph not re-executing after param change
The `useEffect` in react-flow-editor.tsx tracks `paramsVersion` but the comparison logic may not detect param-only changes correctly.

```typescript
// Line 485-513 in react-flow-editor.tsx
const currentGraph = JSON.stringify({
    nodes: nodes.map((n) => ({
        id: n.id,
        typeId: n.data.typeId,
        params: getNodeParamsSnapshot(n.id),  // <-- Should include updated params
    })),
    edges: ...
});

if (currentGraph !== prevGraphRef.current) {
    triggerOnChange();  // <-- Should fire when params change
}
```

**Status**: This looks correct but needs runtime verification.

#### Hypothesis B: Pattern not saved before compositor runs
The compositor (`composite_track`) loads the pattern graph from the **database**:

```rust
// compositor.rs line 333
let graph_json = fetch_pattern_graph(&db.0, annotation.pattern_id).await?;
```

If the user changes the selection query but doesn't save (Cmd+S), the compositor will use the old stored graph with an empty `{}` query.

**Status**: Likely the issue - need to verify if user saved the pattern.

#### Hypothesis C: Serde deserialization mismatch
The SelectionQuery uses `#[serde(rename_all = "camelCase")]`:

```rust
// models/groups.rs
pub struct SelectionQuery {
    pub type_filter: Option<TypeFilter>,    // serializes as "typeFilter"
    pub spatial_filter: Option<SpatialFilter>, // serializes as "spatialFilter"
    pub amount: Option<AmountFilter>,
}
```

Frontend sends:
```json
{"typeFilter": {"xor": ["moving_head"], "fallback": []}, "amount": {"mode": "all"}}
```

**Status**: Verified TypeScript bindings match Rust model - should work.

### 4. Default Behavior Confirmation

When `SelectionQuery` has no filters set (empty `{}`), the backend returns ALL fixtures:

```rust
// services/groups.rs line 196-200
let type_filtered = if let Some(type_filter) = &query.type_filter {
    resolve_type_filter(&groups, type_filter, rng_seed)
} else {
    groups  // <-- Returns ALL groups when no type filter
};
```

This is **intentional** - an empty query means "select everything".

---

## Next Steps

1. **Run and inspect logs** - confirm the select node prints raw/parsed query and fixture counts.
2. **Add console.log in frontend** to verify the query is being set correctly (if backend logs show `{}`).
3. **Check if pattern was saved** - the compositor uses the stored graph in DB.
4. **Verify the onChange chain** - confirm `triggerOnChange()` is called after param updates.

## Quick Test

Ask user to:
1. Configure a type filter in the selection query builder (e.g., add "Moving Head" to XOR types)
2. **Save the pattern** (Cmd+S or click Save button)
3. Re-run the compositor
4. Check if selection is now filtered

If this works, the issue is that changes aren't being saved before the compositor runs.

---

## Files Modified This Session

1. `src-tauri/src/node_graph/nodes/selection.rs`
   - Removed `log::debug!` that was causing compile error
   - Previously fixed JSON string parsing for selection_query param
   - Added debug-only `println!` to log raw/parsed selection query and fixture count

2. `src/features/universe/components/grouped-fixture-tree.tsx`
   - Previously rewrote to remove axis info, add drag-and-drop, double-click rename

3. `src/features/universe/components/selection-query-builder.tsx`
   - Previously fixed infinite loop by using useRef and explicit handlers
