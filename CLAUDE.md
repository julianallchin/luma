Read AGENTS.md before starting any task.

# Working relationship

We are design partners on this project, not client and contractor. Treat the codebase like it's yours, because it is: you co-own the architecture and you're accountable for its long-term shape, not just the diff in front of you.

**Conversation style.** Talk like a peer — casual, direct, lowercase-fine, no corporate prose, no hedging, no flattery. Push back when the framing is wrong; say "that's the hard part and here's why" instead of agreeing your way through. Concise beats thorough-sounding. Dead prose, no aphorisms, no flourishes.

**Design standards.** This project's bar is elegance through meticulous design:

- **No architectural debt.** Don't bolt a feature onto the wrong layer because it's faster. If the right seam doesn't exist yet, building the seam is part of the task. Settle the design before writing code — a decision made in conversation is worth more than a half-wired implementation.
- **No duplication.** Before adding anything, find the existing mechanism that already does it (this codebase usually has one — PCA in circle_fit, frontmatter parsing in agent-loader, the ghost-stack sizing pattern). Extract and share; never copy. If a list/enum/contract exists in more than one place, that's a bug to flag, not a pattern to extend.
- **Primitives over composites.** Prefer a small closed vocabulary that composes (adsr + beat_pulses, parametric motion curves, the grey ladder) over one-off special cases. If a new feature can't be expressed as composition of existing primitives, question whether the primitive set or the feature is wrong.
- **Flag smells even when unasked.** Dead columns, drifted duplicate lists, silent-failure stubs, axis-convention mismatches — surface them when you find them, adjacent to whatever you're doing. Ignoring a known smell is how debt compounds.
- **One canonical way.** One button style, one attribute list, one bar-boundary definition, one UV convention. When you add the second way to do something, you've broken the design — unify instead.

# Ousterhout guidelines (A Philosophy of Software Design)

These are the working vocabulary for design review here — cite them by name when flagging or defending a design.

- **Complexity is incremental.** No single change is the problem. Zero tolerance.
- **Three symptoms:** change amplification, cognitive load, unknown unknowns.
- **Two causes:** dependencies, obscurity.
- **Modules should be deep:** small interface, large implementation. Shallow module = interface cost ≈ implementation benefit.
- **Pull complexity downward.** Implementer eats it, not the N callers.
- **Define errors out of existence.** Best exception handling is an API where the error can't occur.
- **Different layer → different abstraction.** Pass-through methods/variables are a smell.
- **General-purpose beats special-purpose,** somewhat. Special-purpose APIs leak the caller's use case.
- **Design it twice.** Two radically different designs, then pick.
- **Comments describe what code can't:** invariants, contracts, rationale. Write them *first* — they're a design tool. Keep them tight: no history ("was X, now Y", "replaces the old…"), no narrating the change that introduced them — just the nuance and design choice the code can't say. A comment that only makes sense during the review of its diff shouldn't survive the diff.
- **Strategic > tactical.** ~10–15% overhead as continuous investment.

## Rust application

**Deep modules**
- `lib.rs` as facade: `pub use` a curated surface; everything else `pub(crate)`.
- Default to private. `#[non_exhaustive]` on public enums/structs to keep the interface narrow across semver.
- Sealed traits (`mod private { pub trait Sealed {} }`) when a trait is an abstraction, not an extension point.

**Information leakage**
- Return `impl Iterator<Item = T>` instead of `Vec<T>` — the collection is implementation.
- Newtype over exposed primitives: `struct UserId(u64)`, not `u64`.
- Don't expose `Arc<Mutex<Inner>>`. Sync strategy is implementation; export `&self` methods.

**Errors out of existence** (highest leverage in Rust)
- Type-state: `Builder<Unvalidated> → Builder<Validated>`; `Connection<Open>` has `send`, `Connection<Closed>` doesn't.
- Parse, don't validate. `NonZeroUsize`, `NonEmpty<T>`, refinement newtypes with private fields + `TryFrom` constructor.
- Prefer total functions: `truncate`/`saturating_sub`/`get() -> Option` over panicking variants.
- Result of the above: fewer `Result`s in the API, not more.

**Pull complexity downward**
- `impl AsRef<Path>`, `impl Into<String>` in params — caller doesn't convert.
- `Default` + builder for configuration; no 9-arg `new`.
- Blanket `From` impls so `?` works at call sites without `map_err`.

**Layers**
- One error enum per module boundary (`thiserror`), `#[from]` for cause chains. Do *not* mint a 1:1 wrapper error per layer — that's a pass-through.
- `anyhow`/`eyre` only at the binary edge.
- If a fn body is a single delegating call with identical signature, delete the layer.

**Temporal decomposition (anti-pattern)**
- Don't split modules into `parse/`, `validate/`, `execute/` when all three know the same schema. Split by knowledge, not by chronology.

**Obviousness**
- New crates start with `#![warn(missing_docs)]` and `#![warn(clippy::pedantic)]` (existing crates adopt incrementally — don't detonate the build).
- Doc sections are contracts: `# Errors`, `# Panics`, `# Safety`. Write the doc comment before the body; if it's hard to write, the interface is wrong.
- `unsafe` blocks require a `// SAFETY:` comment naming the invariant upheld.
- Doctests as the API's first consumer — they surface shallow/awkward interfaces immediately.
- `cargo doc --no-deps` before finishing a crate touch — broken intra-doc links and rustdoc-lexed code fences are rot that build and clippy both wave through.

**Cheap heuristics**
- Public fn signature longer than 2 lines → probably shallow or leaking.
- Generic params >2 without justification → complexity pushed upward.
- Caller must call A before B → encode in types or merge.

# UI design system

The UI is intentionally minimal — "brutalist instrument panel." Surface language is a monochrome value ladder; depth comes from stacked planes separated by darker trim, not from shadows, gradients, or motion. Treat every new component as part of a hardware-control surface — labels look like silkscreen on a panel, controls feel like slabs you can press, transitions are absent. The rules below are not stylistic preferences; they are the contract.

## Color ladder (dark mode is the canonical mode)

Six greys carry the entire visual hierarchy. No hue except for *meaning* (status dots, primary accents). Defined in `src/App.css` and surfaced as Tailwind tokens via `@theme inline`.

- `rgb(14 14 14)` — `--titlebar-background` — the deepest plane, top bar only
- `rgb(33 33 33)` — `--trim` (Tailwind `bg-trim` / `border-trim`) — fine gaps and gutters between sections, *not* a fill color for content
- `rgb(25 25 25)` — `--gutter` (Tailwind `bg-gutter` / `border-gutter`) — heavier gap / empty-area contrast, one notch deeper than `--trim`
- `rgb(39 39 39)` — `--background`, `--card` — the app body and card surfaces
- `rgb(43 43 43)` — `--stripe` (Tailwind `bg-stripe`) — alternating list-row stripe paired with `bg-card`
- `rgb(46 46 46)` — control fill (buttons, dropdown triggers, menu items at rest)
- `rgb(59 59 59)` — `--hover` (Tailwind `bg-hover` / `hover:bg-hover`) — universal hover fill, used by every interactive surface (buttons, dropdown items, list rows). Don't hardcode a one-off hover color.
- `rgb(8 8 8)` — control border

If you need depth between two adjacent surfaces, the answer is **a slice of `--trim` between them**. Not a shadow, not a border-radius, not a tint shift.

## Components — one canonical style each

There is one button style, one dropdown style, one select style. The shared visual is defined in `src/shared/components/ui/button.tsx` as `BUTTON_CLASS`; every interactive surface composes against it.

- **`<Button>`** (`src/shared/components/ui/button.tsx`) — single style, no variants, no sizes. Don't add a `variant` or `size` prop, don't pass `className="h-X"` to override height. The shape is `h-6 px-2 border rounded-none`, text is `text-[9px] uppercase tracking-wider font-bold`, fill is `rgb(46 46 46)` on `rgb(8 8 8)` border, hover is `bg-hover` (i.e. `--hover`). `AlertDialogAction` / `AlertDialogCancel` and the input-group button all reuse `BUTTON_CLASS`.
- **`<Dropdown label items />`** (`src/shared/components/ui/dropdown.tsx`) — for *actions* (sign out, import from, etc.). Items render in a menu whose trigger is auto-sized to fit the widest item via a pure-CSS ghost-stack — see the "self-sizing geometry" note below. Trigger lays out `label` on the left and a `ChevronDown` on the right with `gap-2` minimum. No content animation, no focus ring.
- **`<Select>` primitives** (`src/shared/components/ui/select.tsx`) — the one true value-picker, a brutalist-styled Radix Select (square, control fill, no animation / shadow / focus ring; content fused to the trigger width via `--radix-select-trigger-width`). Use the compound form (`<Select><SelectTrigger><SelectValue/></SelectTrigger><SelectContent><SelectItem/>…</SelectContent></Select>`) when you want to set the trigger width yourself (`w-full`, `w-28`, …).
- **`<Selector value onChange options />`** (`src/shared/components/ui/selector.tsx`) — options-array shorthand over `<Select>` for *picking one of N states*. It feeds the options to `SelectTrigger`'s `sizingOptions` prop, which renders a pure-CSS ghost-stack (see "self-sizing geometry") so the trigger is sized to the widest option and stays stable across selection changes — use this for a row of selects that must stay aligned. Reach for raw `<Select>` instead when an explicit width is wanted.
- **`<Dropdown label items />`** is for *actions* (sign out, import from, …), not value selection — see above. **Raw `<DropdownMenu>` primitives** (`src/shared/components/ui/dropdown-menu.tsx`) are available for menus that need icons, separators, destructive items, etc.

When adding a new control type (toggle, input, etc.), match the same ladder: square, dark fill on darker border, uppercase 9px, no motion. Don't introduce a parallel design language.

## Hard rules

- **No `rounded-*` other than `rounded-none`.** Corners are square everywhere.
- **No animations on interactive transitions.** Dropdowns / popovers / menus appear and disappear instantly. `transition-colors` on hover is fine; `animate-in` / `slide-in-*` / `fade-*` are not.
- **No focus rings.** `BUTTON_CLASS` already neutralizes them. Don't add them back.
- **No shadows or gradients.** Depth is value steps + trim.
- **No `variant=` or `size=` on `<Button>`.** They were removed deliberately. Use `className` only for layout (e.g., `w-full`).
- **Uppercase + tracking on all controls.** Don't add a sentence-case button.
- **Don't introduce new greys.** Reuse from the ladder above. If you genuinely need a new value, add it to `App.css` with a name, document it here.

## Self-sizing geometry

`<Dropdown>` and `<Selector>` auto-size the trigger to fit the *widest menu item plus the chevron*, with pure CSS (no JS measurement, no manual width). The trigger is a 1×1 grid containing two overlapping cells: a visible "label + chevron" row, and an invisible stack of every item rendered as "item + chevron." The grid cell sizes to the widest child, so the trigger is stable across label changes (selecting a different option in a `<Selector>` does not resize the trigger). The menu's content uses `w-[var(--radix-dropdown-menu-trigger-width)]` so it matches the trigger exactly. If you build another self-sizing control, reuse this pattern.

## Window chrome

The window is `decorations: false` on all platforms; native traffic lights are gone. Custom minimize / maximize / close buttons live in `src/shared/components/window-controls.tsx`. The titlebar (`src/App.css` `.titlebar`) is `rgb(14 14 14)`, drag-region, no border-bottom — it reads as the top edge of the same surface as the body, separated only by value, not by a line. Every screen that renders a titlebar must also render `<HeaderActions>` (settings + account + window controls) — see `src/shared/components/header-actions.tsx`.

## Album art / large media

Album art is *not* base64-inlined in list responses (see `list_tracks_enriched` in `src-tauri/src/services/tracks.rs`). The backend returns the file path on disk; the frontend loads it via `convertFileSrc(track.albumArtPath)` so the browser can lazy-decode and cache it. The Tauri asset protocol is scoped to `$APPCONFIG/tracks/art/**` in `tauri.conf.json`. Don't reintroduce base64 inlining for bulk endpoints — it makes first paint slow and causes blank rows during virtualized scroll.

# Agent friction log

Hit friction — a command that lies, a convention that reads backwards, a trap that ate
twenty minutes — and append one gripe line to `docs/AGENT_FRICTION.md` before you finish:
newest first, `- [YYYY-MM-DD] <gripe>`. Subagents grumble, the lead writes it down. Product
bugs aren't friction; those go in the task report.
