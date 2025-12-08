# Harmony Analysis and Spatial Attributes

This document summarizes recent enhancements to the lighting control system, focusing on improved spatial attribute handling and a significant upgrade to the harmony analysis capabilities. These changes enable more sophisticated and musically reactive lighting effects.

## 1. Enhanced Spatial Attributes and Fixture Rotation

**Problem:** Initially, getting fixtures to react based on their spatial position (e.g., creating an effect that sweeps across a row of lights) was challenging. Specifically, multi-head fixtures like the Venue Tetra Bar had their individual heads incorrectly positioned. Furthermore, fixture rotations were not being applied to head positions, leading to `rel_y` (relative Y) attributes not working as expected for vertically mounted fixtures.

**Solution:**
1.  **Head Position Calculation (Backend - `src-tauri/src/fixtures/layout.rs`):** The logic for computing head offsets within a fixture was improved. It now intelligently distributes logical heads across the physical layout cells, even if the number of heads is less than the number of physical cells (e.g., 4 logical heads over 12 physical pixels on a bar).
2.  **Rotation Application (Backend - `src-tauri/src/schema.rs`):** The `select` node handler now correctly applies the fixture's `rot_x`, `rot_y`, and `rot_z` (Euler ZYX convention) to each individual head's local offset when computing its global `(pos_x, pos_y, pos_z)`. This ensures that if a fixture is rotated (e.g., a horizontal bar rotated 90 degrees to be vertical), its heads will correctly vary along the global Y-axis.
3.  **`Get Attribute` Node (Frontend - `src/shared/lib/react-flow/get-attribute-node.tsx`):** The frontend component for the `Get Attribute` node was upgraded from a plain text input to a user-friendly dropdown menu, allowing easy selection of attributes like `pos_x`, `pos_y`, `pos_z`, `rel_x`, `rel_y`, `rel_z`.

**Impact:**
*   `rel_y` now accurately reflects the vertical position of heads within a selected fixture, even if the fixture itself is rotated.
*   The "Wig Out" issue when selecting multiple fixtures for a linear effect is resolved, as the relative attributes now correctly follow the fixture's actual orientation.

## 2. Enhanced Math Node with Absolute Difference (`abs_diff`)

**Problem:** Creating symmetric spatial effects, like a "moving dot" or "line" that peaks at a specific location, was difficult with only basic arithmetic operations.

**Solution:**
1.  **Backend (`src-tauri/src/schema.rs`):** The `math` node's execution logic was extended to include a new operation: `abs_diff` (absolute difference). This calculates `|A - B|`.
2.  **Frontend (`src/shared/lib/react-flow/math-node.tsx`):** "Absolute Difference" was added as an option to the `math` node's operation dropdown.

**Impact:** This enables precise control over creating effects where the output is highest when two signals are closest (e.g., current beat phase vs. fixture position), providing a natural fall-off rather than a hard edge.

## 3. Upgraded `View Signal` Node for Multi-Dimensional Data

**Problem:** The previous `View Signal` node flattened all incoming `Signal` data into a single 1D array, making it impossible to visualize multi-primitive (`N > 1`) or multi-channel (`C > 1`) signals, which are crucial for debugging spatial and harmony effects.

**Solution:**
1.  **Backend (`src-tauri/src/models/schema.rs`, `src-tauri/src/schema.rs`):** The `RunResult` structure and the `view_signal` node's execution logic were updated to pass the full `Signal` struct (containing `n`, `t`, `c`, and `data`) directly to the frontend without flattening.
2.  **Frontend (`src/shared/lib/react-flow/view-channel-node.tsx`):** The rendering logic was completely rewritten to interpret the `Signal`'s dimensions:
    *   It now draws `N` separate lines (if `N > 1`) to visualize individual primitive values over time.
    *   If `N = 1` and `C > 1`, it draws `C` separate lines (one per channel).
    *   It handles both time-varying (`T > 1`) and static (`T = 1`) signals, drawing curves or flat horizontal lines as appropriate.

**Impact:** `View Signal` is now a powerful debugging tool, allowing developers to visually inspect the exact output of spatial, temporal, and multi-channel signals, making complex graph debugging significantly easier.

## 4. High-Fidelity Probabilistic Harmony Analysis

**Problem:** The previous "Harmony Analysis" (`harmony_analysis`) node relied on pre-segmented chord sections (e.g., "0-5s is C Major"), resulting in hard, blocky, and often delayed chord changes. It did not provide the nuanced, frame-wise probabilities needed for advanced musically reactive lighting.

**Solution:**
1.  **Database Migration:** Added a `logits_path` column to the `track_roots` table (via `src-tauri/migrations/app/20251206120000_add_logits_path.sql`).
2.  **Python Worker (`src-tauri/python/ace_chord_sections_worker.py`):**
    *   Now accepts multiple audio files (e.g., `bass.wav` and `other.wav`) for analysis.
    *   Mixes these stem files on-the-fly before feeding them to the ACE model.
    *   Extracts raw `root_logits` (12 pitch classes + 'no chord' class) frame-by-frame.
    *   Saves these raw float32 probabilities to a `.logits.bin` sidecar file and includes its path in the JSON output.
    *   **(Bug Fix):** Restored missing `min_chord_dur` argument.
3.  **Rust Workers (`src-tauri/src/root_worker.rs`, `src-tauri/src/tracks.rs`):**
    *   `root_worker::compute_roots` now accepts multiple audio file paths.
    *   `tracks.rs::run_import_workers` now ensures stem separation completes *before* harmony analysis. It then constructs the paths to the `bass.wav` and `other.wav` stems and passes *these* to the harmony analysis worker. This ensures a cleaner harmonic signal.
    *   The `logits_path` is correctly persisted to the database.
4.  **Rust (`src-tauri/src/schema.rs` - `harmony_analysis` node):**
    *   Prioritizes loading the `logits_path` from the database.
    *   Reads the raw binary float32 probabilities.
    *   Applies a **Softmax** function to convert logits into true probabilities (0.0-1.0 confidence for each of the 12 pitch classes).
    *   Resamples this high-resolution probabilistic chroma signal to match the graph's time context.
    *   Falls back to the old chord section rasterization if a logits file is not found.

**Impact:**
*   **Cleaner Analysis:** By analyzing only `bass + other` stems, the harmony analysis is far less polluted by percussive noise or vocal melody, leading to more stable and musically accurate chord detection.
*   **Probabilistic Output:** The `harmony_analysis` node now outputs a dense `C=12` signal of probabilities for each note. This unlocks sophisticated effects such as "Harmonic Heatmaps," "Tension-Based Intensity," and "Key-Aware Coloring," where color and intensity can smoothly morph with the music's harmonic flow rather than snapping between hard chord changes.

These combined improvements lay the groundwork for a much richer and more dynamic lighting experience, deeply integrated with the musical structure.
