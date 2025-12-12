# Repository Guidelines

Luma is a Tauri desktop app: a React/TypeScript frontend in `src/` backed by a Rust core in `src-tauri/`. Most features span both halves via Tauri `invoke()` commands and shared TypeScript bindings.

## Project Structure & Module Organization

- `src/`: React 19 + TypeScript UI, Zustand stores, React Flow-based graph editors, Tailwind styling.
  - Feature modules live in `src/features/*` (e.g., `patterns/`, `track-editor/`, `app/`).
  - Shared UI/components in `src/shared/`.
- `src-tauri/`: Rust backend (entry `src-tauri/src/main.rs`, app setup in `src-tauri/src/lib.rs`).
  - Models in `src-tauri/src/models/`.
  - SQLite + migrations in `src-tauri/migrations/{app,project}`.
  - Python workers in `src-tauri/python/` for beats/roots/stems analysis.
- `projects/`: example venue projects (`*.luma`).
- `resources/fixtures/`: fixture definitions bundled into the app.
- `experiments/`: research notebooks and test data.

## Build, Test, and Development Commands

Use **Bun only** for JS tooling.

- `bun install`: install JS deps.
- `bun run dev`: run Vite dev server.
- `bun run tauri dev`: run desktop app with hot reload (Vite + Tauri).
- `bun run build`: typecheck + build frontend to `dist/`.
- `bun run tauri build`: produce distributable desktop build.
- `bun run lint`: Biome lint for TS + `cargo clippy` for Rust.
- `bun run format`: Biome format + `cargo fmt`.
- `cargo test --manifest-path src-tauri/Cargo.toml`: run Rust tests and regenerate TS bindings (see below).

## Coding Style & Naming Conventions

- TypeScript/React: formatted and linted by Biome (`biome.json`). Prefer functional components, hooks, and Zustand stores named `use-*-store.ts`. Files and folders are generally kebab-case; components are PascalCase.
- Rust: standard `rustfmt` + `clippy`. Keep backend modules cohesive around domains (`tracks`, `patterns`, `annotations`, `host_audio`).
- Cross-boundary API: add/rename Tauri commands in `src-tauri/src/lib.rs` and update frontend call sites under `src/features/**`.

## TypeScript Bindings (`ts-rs`)

Bindings are auto-generated from Rust structs in `src-tauri/src/models/` using `ts-rs`.

- Output file: `src/bindings/schema.ts` (ignored by git).
- Regeneration: happens automatically on `cargo test`.
- If you change exported Rust models, run `cargo test --manifest-path src-tauri/Cargo.toml` to refresh bindings.
- Do **not** commit generated `src/bindings/schema.ts`.

## Testing Guidelines

There is no dedicated JS test suite yet. Validate UI changes manually via `bun run tauri dev`. Rust changes should be covered with `cargo test` when possible. Keep migrations consistent with model changes.

## Data & File Locations

The global library database `luma.db` is stored in the Tauri app config directory:

- macOS: `~/Library/Application Support/com.luma.luma/luma.db`
- Windows: `%APPDATA%\\com.luma.luma\\luma.db`
- Linux: `~/.config/com.luma.luma/luma.db`

Venue projects are SQLite `.luma` files, created/opened via the UI and stored wherever the user chooses (samples in `projects/`).

## Commit & Pull Request Guidelines

Follow conventional, imperative commits (e.g., `add track annotation drag`, `fix waveform cache`). PRs should include:

- a clear summary of behavior changes,
- linked issues or context,
- screenshots/video for UI changes,
- notes on any schema/migration impacts.

---

This document serves as the high-level conceptual overview of **Luma**. It ignores the mathematical mechanics in favor of explaining the architectural philosophy and the user experience of the system’s two distinct halves: **The Library** and **The Venue**.

---

# LUMA: Conceptual System Design

## 1. The Core Philosophy: Semantic Lighting

The fundamental problem with current lighting control software is that it is strictly "literal." When you program a show on a traditional console, you are recording specific instructions for specific hardware (e.g., "Turn fixture #101 to 50% brightness"). If you move that show to a different venue with different hardware, the instructions become meaningless.

Luma solves this by introducing a **Semantic Layer**. Instead of recording hardware instructions, Luma records _intent_. It treats a light show like a musical score rather than a mechanical recording.

- **The Musical Score (Intent):** A sheet of music says "Play a C-Major Chord." It does not specify whether that chord is played on a Grand Piano, a Synthesizer, or a Guitar. The intent remains the same regardless of the instrument.
- **The Luma Score:** Similarly, Luma records "Play a Red Circular Pulse." It does not care if the venue has 50 lights or 50,000 lights. It is the job of the venue to interpret that command.

This separation allows for a **"Write Once, Perform Anywhere"** workflow, which is the prerequisite for training an Artificial Intelligence to understand lighting design.

---

## 2. The Global Library (`luma.db`)

_The "What" / The Artist’s Domain_

The Global Library is the heart of the user's creative identity. It is a persistent database that travels with the user, containing their music, their analysis data, and their artistic decisions. It is completely blind to hardware.

### The Vocabulary (Pattern Registry)

To communicate intent, the system relies on a fixed vocabulary of concepts, known as the **Pattern Registry**. These are the "words" in the language of Luma. A pattern might be named "Strobe*Chaos" or "Warm_Wash_Fade."
Crucially, the Global Library only stores the \_definitions* of these patterns (their names and the parameters they accept, like speed or color), not how they are achieved technically. This ensures that the vocabulary remains consistent across every project.

### The Score (Track Annotations)

When a user imports a song, they engage in "Annotation." This is a process similar to video editing. The user places blocks of patterns onto a timeline synced to the audio.
The user is defining a narrative: "During the intro, use _Atmosphere_Blue_. At the drop, switch to _Strobe_Impact_."
This data structure—the pairing of an Audio File with a sequence of Abstract Pattern Tags—is the "Golden Dataset." It is the clean, structured data required to fine-tune an Omni-LLM. The AI learns to associate the audio characteristics of a "Bass Drop" with the semantic token "Strobe_Impact."

---

## 3. The Venue Project (`project.luma`)

_The "How" / The Engineer’s Domain_

The Venue Project is a local, disposable file created for a specific physical location. It acts as the translator between the abstract commands of the Global Library and the physical reality of the stage.

### The Implementation Layer

If the Global Library asks for a "Red Circular Pulse," the Venue Project must answer the question: _"How do we do that with this specific pile of equipment?"_
The engineer builds an **Implementation** for that pattern. This is a logic container where the "idea" of the pattern is connected to the actual lights. This allows for infinite creative interpretation.

- In a small club, "Red Circular Pulse" might be implemented as a simple chase across 4 ceiling lights.
- In a stadium, the same "Red Circular Pulse" command might trigger a complex 3D sweep across massive LED screens and moving heads.
  The song file driving them remains identical in both cases.

### The Semantic Grouping

To avoid getting bogged down in DMX addresses, the Venue Project organizes physical lights into **Semantic Groups**. The Engineer tags hardware as "The Ceiling," "The Floor," or "The Blinders."
The Implementations target these groups by name. This ensures that if the venue changes (e.g., a light breaks and is replaced), the Engineer only updates the Group definition, and every pattern in the system automatically adapts to the new hardware.

---

## 4. The Compositing Engine

_The Real-Time Conductor_

Since Luma is not playing back pre-recorded DMX frames, it must generate the lighting data in real-time, similar to a video game engine rendering graphics. This is the job of the Compositor.

### The Layering System

Lighting is rarely just one thing happening at once. Luma treats patterns as transparent layers that can be stacked.

- **Layer 1 (Base):** A slow, atmospheric color wash.
- **Layer 2 (Rhythm):** A beat-synced pulse.
- **Layer 3 (Impact):** A blinder hit on the snare drum.

### Blending and Conflict Resolution

When multiple layers try to control the same lights, the Compositor resolves the conflict using **Blending Modes**.

- If the mode is **"Add,"** the lights get brighter (the blinder adds to the wash).
- If the mode is **"Subtract,"** the lights get darker (ducking the lights when the kick drum hits).
- If the mode is **"Replace,"** the top layer takes over completely (a spotlight overrides the background).

This allows for complex, dynamic behaviors to emerge from simple building blocks. A "Ducking" pattern doesn't need to know what color the lights are; it simply applies a "darkness" mask to whatever is happening underneath it.

---

## 5. The Future: AI Generative Workflow

The strict separation of `luma.db` and `project.luma` is the architectural enabler for the "Secret Goal."

Because the **Global Library** is free of hardware noise (it doesn't contain DMX addresses or fixture types), it becomes a pure training ground. We can feed the AI thousands of songs and their corresponding "Scores."
Eventually, the workflow shifts:

1.  User drops a new song into Luma.
2.  The AI analyzes the audio.
3.  The AI generates the "Score" (the Track Annotations) by predicting which abstract patterns fit the song's structure.
4.  The User loads this score into the "Venue Project."
5.  The Venue Project's logic instantly renders that AI-generated score onto the physical lights.

This achieves the holy grail: Instant, high-quality, musically synchronous lighting for any track, on any stage, without manual programming.
