---
title: Selection And Hierarchy Design
---

# Selection, Hierarchy, and Applying Capabilities

## The "Leaf Node" Selection Problem

We want to select "things" to apply effects to.
- For a generic dimmer: we select the bulb.
- For a matrix: we might select a specific pixel.
- For a moving bar: we might select a specific pixel for color, but the *whole bar* for movement.

If our Selection wire only carries "Leaf Nodes" (e.g., Pixel 3 of Bar 1), and we plug that into a `Pan` node, the system needs to know: *"Pixel 3 cannot pan itself. But its parent (Bar 1) CAN."*

## Hierarchy Resolution Strategy

We need a mechanism to **traverse up the hierarchy** to find the capability.

### 1. The Hierarchy
Every selectable item exists in a tree:
*   **Root**: The Universe / Stage
*   **Fixture**: The Physical Unit (e.g., "Venue Tetra Bar #1")
*   **Head/Primitive**: The independent controllable part (e.g., "Pixel #3")

### 2. Selection = Intention
When you select "Pixel #3" and plug it into a "Red Color" node, your intention is: *"Make Pixel 3 Red."*
When you select "Pixel #3" and plug it into a "Tilt" node, your intention is: *"Tilt the thing that holds Pixel 3."*

### 3. Resolution Logic (The "Bubble Up" Algorithm)
This logic belongs in the `Apply` node (or the DMX Renderer), not the `Select` node.

**The Algorithm:**
When applying Capability $C$ (e.g., Pan) to Selection Item $S$ (e.g., Pixel 3):
1.  Does $S$ have a DMX channel for $C$?
    *   **YES**: Write to that channel. (e.g., Pixel 3 has Red). Done.
    *   **NO**: Check Parent of $S$.
2.  Does $Parent(S)$ have a DMX channel for $C$?
    *   **YES**: Write to that channel. (e.g., Bar 1 has Tilt). Done.
    *   **NO**: Check Parent...

**Crucial Detail: Conflict Resolution**
If you select "Pixel 1" AND "Pixel 2" (same bar) and apply "Tilt":
*   The logic bubbles up for *both*.
*   Both resolve to "Bar 1 Tilt".
*   The system writes the value twice.
*   Since it's the same value (assuming coherent pattern), it's fine.
*   If the values differ (e.g. a gradient across pixels applied to Pan), we have a physical conflict.
    *   *Strategy*: **Last Write Wins** or **Average**. For a moving bar, averaging the "desired tilt" of all its selected pixels is actually a very intuitive behavior!

## The "Tree View" Editor (UI)

You described a UI for editing the selection:
*   Searchable list of fixtures.
*   Collapsible rows.
*   Leaf nodes are selectable.

### Data Requirements for UI
The backend needs to send a `Hierarchy Manifest` to the frontend.

**Structure:**
```json
[
  {
    "id": "fixture-1",
    "label": "Venue Tetra Bar 1",
    "type": "Fixture",
    "selectable": true,  // Can select whole bar
    "children": [
      { "id": "fixture-1:head-0", "label": "Pixel 1", "selectable": true },
      { "id": "fixture-1:head-1", "label": "Pixel 2", "selectable": true },
      ...
    ]
  },
  {
    "id": "fixture-2",
    "label": "Moving Head 1",
    "selectable": true,
    "children": [] // No children, it's atomic
  }
]
```

### Backend "Select" Node Update
*   **Param**: `selection_ids` (List of strings).
*   **Execution**:
    *   Iterate IDs.
    *   If ID is `fixture-X`, expand to ALL heads (if implicit selection desired) OR just emit `fixture-X` and let the hierarchy solver handle it?
    *   **Decision**: **Always emit Leaf Nodes**.
        *   If user selects "Tetra Bar 1" (Container), the node outputs `[Head 0, Head 1, Head 2, Head 3]`.
        *   If user selects "Pixel 1", node outputs `[Head 0]`.
        *   *Why?* Because `GetAttribute("Index")` needs to work on pixels. If we output the "Bar" as 1 item, index=0. If we output 4 pixels, index=0..3.
        *   *But what about Pan?* The "Bubble Up" logic handles it. The bar receives the average/max/last of the 4 pixels' tilt requests.

## Refined Architecture

1.  **Select Node**:
    *   Stores explicit list of IDs (from tree view).
    *   **Always expands** Fixture IDs into their constituent Head IDs.
    *   Outputs `Selection` = List of Heads (Primitives) with absolute positions.

2.  **Apply Node (e.g. Apply Tilt)**:
    *   Input: List of Heads.
    *   Logic:
        *   For each Head, find its **Effective Tilt Channel**.
        *   (Lookup: Head -> Fixture -> Mode -> Channel Map).
        *   If Head has no Tilt, use Fixture's Tilt.
    *   Result: A map of `DMX_Channel_Address -> Value`.

3.  **Conflict Handling**:
    *   If multiple Heads map to the *same* DMX Channel (e.g. 12 pixels all mapping to the main Pan channel), we must aggregate.
    *   **Aggregation**: `Average` is usually safest for position. `Max` (HTP) for dimmer.

## Summary of Work to Do

1.  **Backend**:
    *   Update `Select` node to accept `selection_ids` (JSON list) instead of `query`.
    *   Implement `get_patch_hierarchy` command for the UI Tree View.
    *   (Already done: Layout solver for positions).
    *   (Future: The "Bubble Up" logic in the DMX Renderer).

2.  **Frontend**:
    *   Build the Tree View component.
    *   Connect it to the `Select` node's inspector.

This feels solid. It handles the "Moving Bar" case elegantly via the Bubble Up + Aggregation strategy.
