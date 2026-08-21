# Pattern-graph canvas — style spec (web reference)

The measured, source-cited description of what the **web (React/Tauri) pattern
graph editor** draws. This is the quality bar for the GPUI port: the GPUI
canvas is done when it reproduces these values and the reference captures next
to it read as the same screen.

Every number below was read off the running page with `getComputedStyle` at
zoom 1 (WebKit, the engine inside the app's WKWebView), not inferred from
Tailwind class names — where a class and the computed value disagree, the
computed value is what ships, and the disagreement is called out.

Reference captures (2x device pixels, WebKit, byte-stable across runs):

| file | what |
| --- | --- |
| `web-gradient-whole.png` | whole `gradient` graph, one `fitView` |
| `web-gradient-closeup.png` | ramp → sample-palette → apply-color, labels readable |
| `web-circle_pill_step-whole.png` | whole `circle_pill_step` graph |
| `web-circle_pill_step-closeup.png` | selector + text-input param controls |

Inputs, identical for both stacks: `fixtures/gradient.json`,
`fixtures/circle_pill_step.json` (graph, view-node signals, closeup viewport)
and `node-types.json` (port/param catalogue). Regenerate with
`bun harness/gauntlet/extract-fixtures.ts`; recapture with
`node harness/gauntlet/shot-graph.mjs --all`.

---

## 1. Canvas

| property | value | source |
| --- | --- | --- |
| canvas background | `#212121` (`--trim`) | `src/shared/lib/react-flow-editor.tsx:842` (`bg-trim`), token `src/App.css:113` |
| React Flow pane / viewport fill | transparent — the trim shows through | `node_modules/reactflow/dist/style.css:13,25` |
| grid / dots | **none** — no `<Background>` component is rendered | `src/shared/lib/react-flow-editor.tsx:843-886` |
| zoom range | 0.5 … 8 (`minZoom` left at React Flow's 0.5 default, `maxZoom={8}`) | `src/shared/lib/react-flow-editor.tsx:883` |
| initial framing | `fitView` on mount | `src/shared/lib/react-flow-editor.tsx:884` |
| node containment | `.react-flow__node { contain: layout }` | `src/App.css:250-252` |

## 2. Node card

Measured on `Linear Ramp` (`ramp_between`, two inputs / one output), zoom 1.

| property | value | source |
| --- | --- | --- |
| fill | `#272727` (`--card`) | `base-node.tsx:154` (`bg-card`), `src/App.css:121` |
| border | `2px solid #191919` (`--gutter`) — all four sides | `base-node.tsx:154` (`border-2 border-gutter`), `src/App.css:114` |
| corner radius | **0px** | `base-node.tsx:154` says `rounded-lg`, but `--radius: 0rem` (`src/App.css:160`) makes `--radius-lg` 0. The class is decorative; the rendered card is square. |
| min width | `170px`; card grows to content | `base-node.tsx:154` |
| clipping | `overflow: hidden` | `base-node.tsx:154` |
| body text | Inter 12px / 16px, `#777777` (`--muted-foreground`) | `base-node.tsx:154` (`text-xs`), `src/App.css:100,132` |
| measured box | 170 × 73.95 (ramp\_between), 172.77 × 109.95 (math), 232 × 109.3 (round) | — |

> **Smell.** `base-node.tsx:154` sets both `text-muted-foreground` and
> `text-foreground` on the card. The computed color is `#777777`
> (muted) — `text-foreground` is dead. Port labels and titles are therefore all
> mid-grey; only explicitly-colored children (`text-foreground` on the audio
> node's track name) come out light.

### Header

| property | value | source |
| --- | --- | --- |
| fill | `#212121` (`--trim`) | `base-node.tsx:156` (`bg-trim`) |
| padding | 4px top / 4px bottom / 8px left / 8px right | `base-node.tsx:156` (`px-2 pt-1 pb-1`) |
| type | Inter 12px / 16px, weight 500, letter-spacing **−0.3px** (`tracking-tight`) | `base-node.tsx:156` |
| color | `#777777` | inherited (see smell above) |
| measured height | 23.98px (24px box incl. padding) | — |
| separator | none — the header is a value step, not a rule | — |

### Port block (body)

| property | value | source |
| --- | --- | --- |
| block padding | 4px top / 4px bottom, no horizontal padding | `base-node.tsx:165` (`py-1`) |
| layout | two independent columns, `justify-content: space-between`, min 8px gutter | `base-node.tsx:165` (`flex justify-between gap-2`) |
| row gap within a column | 6px (`gap-1.5`) | `base-node.tsx:166,179` |
| input rows | `padding: 0 8px 0 16px`, labels left | `base-node.tsx:60` (`pl-4 pr-2`) |
| output rows | `padding: 0 16px 0 8px`, labels right (`justify-end`) | `base-node.tsx:60` (`justify-end pr-4 pl-2`) |
| row height | 15.98px (one 12px/16px line) | — |
| label type | Inter 12px / 16px, weight 400, `#777777` | inherited from card |

Outputs start at the **top** of the card alongside inputs — they do not stack
below the inputs (`base-node.tsx:160-164`).

## 3. Ports

All four pieces of a port share one anchor: `PORT_ANCHOR = 6px` in from the
row's inner edge, vertically centred. The dot's centre *is* that anchor, and
the invisible React Flow handle shares it, so a wire lands exactly in the dot.

| piece | geometry | source |
| --- | --- | --- |
| ring | 9 × 9px circle, `1.5px` solid border in the port hue, transparent fill | `base-node.tsx:25-26,82-90` |
| dot (connected only) | 4 × 4px circle filled with the port hue | `base-node.tsx:27,92-102` |
| ghost lead-in (connected only) | 6 × 2px bar in the port hue at **40% opacity**, from the card edge to the anchor, behind the body | `base-node.tsx:28,64-80` |
| hit target | 14 × 14px transparent handle, no border | `base-node.tsx:29,103-116` |
| unconnected port | bare ring, no dot, no ghost | `base-node.tsx:92,66` |

Ring centring: the element's inner edge is pinned at 6px and pulled back by half
its own size (`translate(∓50%, −50%)`) — `base-node.tsx:48-54`.

### Port-type hues

Canonical table, shared by handles and wires so a wire and its socket read as
one type (`src/shared/lib/react-flow/types.ts:17-29`):

| port type | hex | rgb | tailwind name |
| --- | --- | --- | --- |
| `Intensity` | `#f59e0b` | 245 158 11 | amber-500 |
| `Audio` | `#3b82f6` | 59 130 246 | blue-500 |
| `BeatGrid` | `#10b981` | 16 185 129 | emerald-500 |
| `Series` | `#8b5cf6` | 139 92 246 | violet-500 |
| `Color` | `#ec4899` | 236 72 153 | pink-500 |
| `Signal` | `#22d3ee` | 34 211 238 | cyan-400 |
| `Selection` | `#c084fc` | 192 132 252 | purple-400 |
| `Events` | `#ef4444` | 239 68 68 | red-500 |
| `Stops` | `#f472b6` | 244 114 182 | pink-400 |
| *(unknown type)* | `#6b7280` | 107 114 128 | gray-500 (`DEFAULT_PORT_COLOR`) |

## 4. Wires

| property | value | source |
| --- | --- | --- |
| stroke width | `2` | `react-flow-editor.tsx:450,519,583` |
| stroke color | hue of the **source** port's type | `react-flow-editor.tsx:87-95` |
| shape | horizontal stub out of each port, one straight diagonal between them, both corners filleted | `react-flow/fillet-edge.tsx:8-78` |
| stub length | `16px` | `fillet-edge.tsx:11` |
| fillet radius | `10px`, clamped to half of each adjoining segment | `fillet-edge.tsx:12,45` |
| corner curve | quadratic Bézier with the corner as control point | `fillet-edge.tsx:52` |
| fill | none; no arrowheads, no dash, no animation | `fillet-edge.tsx:100-108`, reactflow `style.css:44-49` |
| drag ("connection line") | same fillet path, stroke = the originating port's hue, width 2 | `react-flow-editor.tsx:157-161`, `fillet-edge.tsx:114-139` |

## 5. Selection

| target | treatment | source |
| --- | --- | --- |
| node | **none** — the app defines no `.selected` node style and the custom node bypasses React Flow's default node CSS | no rule in `src/App.css` or `src/shared/lib/react-flow/*`; reactflow's `.selected` rules only target `__node-default/input/output/group` (`style.css:248-256`) |
| node focus | outline suppressed | reactflow `style.css:226-228` |
| edge | path stroke becomes `#555` — **overriding the port hue** | reactflow `style.css:72-76` |
| selection rectangle | React Flow default (`rgba(0, 89, 220, 0.08)` fill) | reactflow `style.css:267` |

> **Smell / port for GPUI with eyes open.** Node selection is invisible: the
> only way to tell a node is selected is to press Delete. And selecting an edge
> throws away its type color. Neither is a deliberate design decision — both are
> React Flow defaults nobody overrode. The GPUI port should give selection a
> real treatment; it should not copy "no treatment" for fidelity's sake.

## 6. Param controls inside nodes

| element | value | source |
| --- | --- | --- |
| control block padding | 8px left / 8px right / 4px bottom per param | `standard-node.tsx:32,66`, `math-node.tsx:38,55,88` |
| param label | 10px, `#9ca3af` (`text-gray-400`), 4px bottom margin, block | `standard-node.tsx:35-38` |
| text / number input | height **28px** (`h-7`, overriding the canonical `h-6`), fill `#2e2e2e` (`--control`), `1px` border `#080808` (`--control-border`), radius 0, 12px text `#e4e4e4`, 8px horizontal padding | `standard-node.tsx:58,81`; `ui/input.tsx:11-12`; tokens `src/App.css:117-118` |
| selector (enum params) | height **24px** (`h-6`), same control fill and border, radius 0, **9px uppercase bold, letter-spacing 0.45px**, `color: foreground/90`, 8px horizontal padding, 8px gap to a 12 × 12px chevron at 50% opacity | `ui/selector.tsx:40,52-57`; `ui/select.tsx:42,48-52` |
| selector width | sized to the widest option by the pure-CSS ghost stack (stable across value changes) | `ui/select.tsx:65-84` |
| slider rows (falloff etc.) | 10px muted label + 10px mono value, 9px muted help text | `react-flow/falloff-node.tsx:28-95` |

> **Smell.** The node param input is `h-7`; every other input in the app is
> `h-6`. Two heights for the same control is one design language too many.

## 7. View node body (`view_signal`)

| property | value | source |
| --- | --- | --- |
| plot area | 720 × 140 CSS px canvas, backed at `devicePixelRatio` | `react-flow/view-channel-node.tsx:12-13,29-36` |
| plot background | `#272727` (`bg-background`) | `view-channel-node.tsx:213` |
| trace | 1.5px round-joined line per series, hue `hsl(i·30, 82%, 62%)` (12-step wheel) | `view-channel-node.tsx:8-11,59-62` |
| axis labels | 10px monospace, `rgba(226,232,240,0.85)`, min/max at 6px inset | `view-channel-node.tsx:54,92-97` |
| legend chip | 9px label + 9px mono value, 8 series max, `border-white/5` on `bg-white/5`, 2px gap | `view-channel-node.tsx:234-257` |
| empty state | centered 11px `waiting for signal data…` in `text-slate-400` | `view-channel-node.tsx:227-229` |
| playhead | 1px `bg-red-500/80`, hidden unless host audio is loaded | `react-flow/base-node.tsx:294-297` |

## 8. Typography

One family everywhere: `Inter, Avenir, Helvetica, Arial, sans-serif`
(`src/App.css:100`). The app ships **no `@font-face` for Inter** — on a machine
without Inter installed the real UI silently renders Helvetica. The harness page
loads the same two TTFs the GPUI side embeds (`harness/fonts.css`,
`harness/fonts/Inter-{Regular,Bold}.ttf`), so the captures are Inter by
construction. The GPUI port should keep embedding the font rather than
inheriting this gap.

Sizes in play: 12px (titles, port labels, inputs), 11px (audio node body,
falloff copy), 10px (param labels, axis, legend), 9px (selector text, legend
values, slider help).

## 9. Grey ladder used by this screen

| token | value | where |
| --- | --- | --- |
| `--gutter` | `#191919` | node border (2px) |
| `--trim` | `#212121` | canvas background, node header |
| `--card` / `--background` | `#272727` | card fill, plot background |
| `--control` | `#2e2e2e` | input / selector fill |
| `--control-border` | `#080808` | input / selector border |
| `--muted-foreground` | `#777777` | titles, port labels |
| `--foreground` | `#e4e4e4` | input text, selector text (at 90%) |
| `text-gray-400` | `#9ca3af` | param labels |

Source: `src/App.css:112-140`.
