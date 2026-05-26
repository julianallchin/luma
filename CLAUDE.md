Read AGENTS.md before starting any task.

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
- **`<Selector value onChange options />`** (`src/shared/components/ui/selector.tsx`) — for *picking one of N states*. Same visual as `<Dropdown>` — it's a thin wrapper. Trigger shows the currently selected option's label.
- **Raw `<DropdownMenu>` primitives** (`src/shared/components/ui/dropdown-menu.tsx`) are still available for cases that need icons, separators, destructive items, etc. The defaults match the `<Dropdown>` look so direct use is fine.

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
