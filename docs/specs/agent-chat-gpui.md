# Agent chat, in Rust and GPUI

Status: both halves are built — §8 records the runtime as shipped, §9 the GPUI
surface. Owns the port of the TypeScript agent stack
(`src/shared/components/agent-chat/**`, `src/shared/lib/agent/**`, the two
agent specs) into Rust, and the GPUI chat surface that drives it.

Companion documents:

- `harness/gauntlet-chat/style-spec.md` — the visual bar, with `file:line`
  sources in the comet (Zeron) clone. Read it before writing any element code.
- `docs/specs/dispatcher-port-guide.md` — how a command gets onto the seam.

---

## 0. The one style exception

Julian's explicit call: **the agent-chat surface steals comet's style and
UX** — transparency, sliding panel motion, polished streaming markdown. The
rest of the app keeps the brutalist no-animation contract in `CLAUDE.md`.

This is a scoped exception, and scoping it is a design obligation, not a
footnote. The mechanism:

- The comet-derived tokens and motion live in `luma-chat`'s own `theme` /
  `motion` modules. `luma-ui::ladder` — the six-grey brutalist ladder — is
  **not** extended, and `luma-chat` does not import it.
- Nothing outside `luma-chat` / `luma-md` may import those tokens. One
  canonical way *per surface*: the app has two surfaces now, each internally
  singular, and the boundary is a crate boundary so a drift is a compile
  error rather than a review catch.
- The chat panel's outer edge against the app is a brutalist seam — a slice
  of `--trim` — so the two languages meet on the app's terms.

If a third surface ever wants comet's language, that is the moment to decide
the app has changed its mind, not the moment to copy tokens a third time.

### License

comet/Zeron is MIT (`LICENSE:1`, © 2026 Wing). Lifting source is permitted
with the notice preserved. Required on any port:

- `gpui/crates/md/THIRD_PARTY/zeron-MIT.txt` — the license verbatim.
- `//! Ported from zeron (MIT, © 2026 Wing) — crates/ui/src/markdown/<file>`
  as the first line of every lifted file.
- Geist / Geist Mono are **not** lifted (SIL OFL, separate notice). The chat
  uses the app's existing Inter + a system mono; if we later want Geist, it
  gets its own `THIRD_PARTY/` entry.

We do **not** add comet or its zed fork as a cargo dependency. `gpui/Cargo.toml`
carries a load-bearing rule: gpui must be a *branch* spec so cargo unifies it
with `gpui-component`'s. A second git source forks gpui and every `Icon` stops
being an `IntoElement`. Ported source only.

---

## 1. Where the agent loop lives

### Candidate A — a `luma_lib` service behind the dispatch seam

The loop is `luma_lib::agent`, a peer of `agent_execution` and
`services::authored_documents`. It reaches tools by calling `dispatch()`
in-process, reuses the existing Rust thread persistence and authored-state
machinery directly, and hands a host a stream of turn events. GPUI subscribes
to that stream and folds it into entity state; the Tauri adapter serializes it
onto the existing event bus.

**For.**

- The loop's dependencies are already there. A turn touches ~20 dispatched
  commands, `PythonWorkspaceService`, `AuthoredDocuments`, the SQLite trigger,
  and `TrackHost`. Every one of those lives in `luma_lib`, most `pub(crate)`.
  Putting the loop anywhere else means widening that surface for one caller.
- **It works without a window.** `auto-light.ts` is a background batch driver
  that runs turns over many tracks. A CLI or a cron job wants the same. A
  loop that needs a GPUI `App` cannot serve any of them, and the day we want
  one we would build the second implementation we are trying to avoid.
- The turn ordering (`persist(user)` → `prompt` → `prepare_turn` →
  `persist(assistant)` → `finalize_turn`) is a **durability protocol**, not
  UI logic. It is resumable, idempotency-keyed and enforced by a database
  trigger. It belongs with the data it protects.
- Testability falls out: a scripted model + a temp database is a plain
  `#[tokio::test]`, no window, no test platform.

**Against.**

- Two hosts means the stream crosses a seam, and one of them (a webview) can
  only receive JSON. Some serialization is unavoidable.
- The tool surface has host-flavoured members today — `set_preview_selection`
  is editor-only state, `preview` renders from the live canvas. Those need a
  home.

### Candidate B — a gpui-side crate talking to the model API directly

`gpui/crates/chat` owns the loop. Tools call `Library` (which already wraps
`dispatch`). Streaming is entity updates with no serialization at all, and
the transcript is a native Rust type designed for the renderer rather than
for a durable JSON column.

**For.**

- No seam. A delta arrives, an entity mutates, `cx.notify()` — that is the
  whole path, and it is the shape gpui wants.
- The GPUI migration is *decided*. If the Tauri webview is going away, "one
  canonical implementation" is satisfied by one implementation **in gpui**
  just as well as by one in `luma_lib`. The duplication argument for A is
  weaker than it first looks.
- Far less design pressure: no typed event vocabulary to invent, no
  host-agnostic tool abstraction, no double-serialization question.
- Ships sooner, and the TS stack keeps working meanwhile.

**Against.**

- Headless is impossible. Background lighting, batch runs, and any future
  server-side turn all die at the door.
- It re-hosts the durability protocol above the database that enforces it.
  The trigger's invariant would be maintained by UI code, which is exactly
  the layer least able to be careful about it.
- The Rust thread persistence, authored-state and workspace layers are ~19k
  lines already written and `pub(crate)`. Candidate B either promotes a large
  chunk of that to the public API or re-reaches it through `dispatch` JSON —
  the second is fine for reads and awful for the workspace/merge dance.
- The tool bodies would be JSON round-trips through `Library` where A calls a
  Rust function.

### Verdict: **A**, with one amendment to the brief.

B's honest strength is that the migration decision undercuts the duplication
argument. It loses anyway on headless: `auto-light` is not a nice-to-have, it
is an existing product feature, and a loop that requires a window cannot run
it. Everything else — trigger proximity, workspace access, test shape — points
the same way.

**Amendment: turn streaming does not go through `EventSink`.**

The brief proposes `Events`/`EventSink` as the streaming path, with GPUI
implementing a sink. That is the obvious seam and it is the wrong one.
`EventSink`'s contract (`dispatch/services.rs`) is a *string-keyed, fire-and-
forget, `serde_json::Value`, app-wide broadcast* whose doc comment says
"emission cannot fail" because a command's write is already committed. Turn
deltas are none of that: they are per-turn, ordered, high-rate, and typed.
Pushing them through it means GPUI deserializes JSON that the loop serialized
one function call earlier, and the subscriber has to filter a global bus by a
thread id stuffed into the event name or the payload.

Instead:

```rust
// luma_lib::agent
/// One turn, as it happens. Dropping the stream cancels the turn.
pub struct TurnStream { /* mpsc receiver + abort guard */ }
impl futures::Stream for TurnStream { type Item = TurnEvent; }

impl AgentService {
    pub fn turn(&self, thread: ThreadScope, prompt: UserPrompt) -> TurnStream;
}
```

GPUI awaits `TurnStream` in a `cx.spawn` and folds into the entity. The Tauri
host owns a ~20-line adapter that drains the same stream into
`Events::emit("agent-turn", …)`. The webview's serialization is the webview
adapter's problem — pull complexity downward, and only the host that needs
JSON pays for it. Both hosts still drive one loop, which was the actual
requirement.

### Where the host-flavoured tools go

`set_preview_selection` and `preview` are the two that look like they need a
bridge back into the host. They do not:

- The graph tools already write through `save_pattern_graph_document` /
  `authored_state_write_workspace_graph`. The live canvas is a *projection*
  of the authored document, not the source. So the host does not receive
  edits — it observes the document. A `TurnEvent::DocumentChanged { revision }`
  is enough; the editor re-reads.
- `preview` is `previewToPngBase64` over an `OffscreenCanvas` today, which is
  the only reason it is host-side. In Rust it is the `image` crate and a
  nearest-neighbour ×4 — it moves backend-side, as the code-execution note
  already listed it should.
- `set_preview_selection` is genuinely ephemeral editor state and is the one
  real exception. It becomes a `TurnEvent::PreviewSelection { expression }`
  the host may honour or ignore. One special case, named, rather than a
  general `Bridge` trait that would invite twelve more.

No `Bridge` trait. If a second ephemeral-UI tool ever appears, that is the
moment to generalize, and by then there will be two examples to generalize
from.

---

## 2. Runtime design (`luma_lib::agent`)

```
src-tauri/src/agent/
  mod.rs          AgentService, TurnStream, the turn protocol
  transcript.rs   AgentChatPart, the fold, the durable-JSON contract
  model/
    mod.rs        ModelClient trait, ModelRequest/ModelEvent, ModelId table
    openrouter.rs openai-completions SSE
    anthropic.rs  anthropic-messages SSE
    scripted.rs   #[cfg(any(test, feature = "scripted-model"))]
  tools/
    mod.rs        Tool trait, registry, JSON Schema
    python.rs skill.rs graph.rs ask_venue.rs subagent.rs
  skills/         *.md, include_str! in path order
  subagent.rs     SubagentManager + AgentLoader (frontmatter)
  prompts/        track.md, graph.md — byte-stable, prompt-cache prefixes
```

### 2.1 The model seam

```rust
/// A streaming chat completion, provider-independent.
///
/// One trait, two transports. `Kimi K3 Fast` is a *model id on OpenRouter*,
/// not a third implementation — provider and model are separate axes, and
/// conflating them is what produced four drifting model-id lists in the TS
/// stack.
pub trait ModelClient: Send + Sync + 'static {
    fn stream(&self, request: ModelRequest) -> BoxStream<'static, Result<ModelEvent, ModelError>>;
}

#[non_exhaustive]
pub enum ModelEvent {
    TextDelta(String),
    ReasoningDelta(String),
    ToolCallStarted { id: ToolCallId, name: String },
    ToolCallArgsDelta { id: ToolCallId, json: String },
    ToolCallEnded { id: ToolCallId },
    StepEnded { stop_reason: StopReason, usage: Usage },
}
```

`ModelEvent` is a **delta** vocabulary. Pi emitted whole-message snapshots and
`applyAgentEvent` diffed them; owning the wire means we get real deltas, and
the streaming veil renderer *needs* to know which characters are new. This is
the one place the port deliberately diverges from the TS shape.

Providers:

| impl | wire | models |
| --- | --- | --- |
| `OpenRouterClient` | `openai-completions` SSE + `HTTP-Referer`/`X-Title` | `anthropic/claude-opus-5`, `moonshotai/kimi-k3-fast`, `x-ai/grok-4.5`, `moonshotai/kimi-k2.6:nitro` |
| `AnthropicClient` | `anthropic-messages` SSE, direct key | `claude-*` |

`gateway-fetch.ts` deletes itself — it exists solely to work around WKWebView
CORS preflight, and a Rust HTTP client has no preflight. The Vercel AI Gateway
provider is dropped unless someone is actually using it; it was only ever the
CORS escape hatch.

**One model table.** `ModelId` is a single `enum` (or one static slice) in
`model/mod.rs` carrying id, provider, display name, and default reasoning
level. The settings picker, the graph agent, the venue expert and the subagent
`model` override all read it. Today those are four independent sources —
`AGENT_MODELS`, `graph-agent.ts:162`'s hardcoded `x-ai/grok-4.5`, `ask-venue`'s
`VENUE_EXPERT_MODEL`, and an unvalidated free-form string — which is three
duplicate-list drifts in one subsystem. Fixing it is part of this port, not a
follow-up.

### 2.2 Keys

localStorage dies. Resolution order, one function, `agent::model::api_key`:

1. `LUMA_OPENROUTER_API_KEY` / `LUMA_ANTHROPIC_API_KEY` — headless, CI, tests.
2. The settings table in the app database, via the existing `set_setting` /
   `AppSettings` path.
3. macOS keychain — deferred, but the resolution function is the one place it
   would land.

This kills the "extract the key from `~/Library/WebKit/.../localstorage.sqlite3`"
hack the headless runs need today. `NotConfigured` names the provider it
actually wanted, unlike today's `"OpenRouter API key is not set."` on both
providers regardless of which is selected.

### 2.3 Tools

```rust
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> Cow<'static, str>;
    /// JSON Schema for the arguments. `schemars` derive on the arg struct.
    fn schema(&self) -> &'static serde_json::Value;
    async fn call(&self, ctx: &ToolContext, args: Value) -> ToolOutcome;
}

/// What a tool result becomes for the model: text, an error, or content
/// blocks (which is how `preview` and matplotlib figures reach a vision
/// model). Mirrors `toModelOutput`.
pub enum ToolOutcome { Text(String), Error(String), Content(Vec<ContentBlock>) }
```

`ToolContext` carries `&AppServices`, the thread id, the turn message id, the
optional `execution_id` / `authored_workspace_id` (this is what makes a tool a
*subagent's* tool — the same tool bound to a different workspace, exactly as
`buildTrackSubagentTools` does), and the abort token.

The subagent surface assertion survives verbatim: a child's tool-name set must
be **exactly** the parent's (`assertSameDomainToolSurface`). In Rust this is
better than an assertion — the child's registry is built by the same function
with a different `ToolContext`, so it cannot differ. Errors defined out of
existence.

Skills are `include_str!` in path-sorted order so the tool description stays
byte-stable (prompt caching). Same for the two system prompts, which are moved
to `prompts/*.md` verbatim from `build-context.ts:19-79` and
`graph-agent.ts:136-159`.

### 2.4 The transcript, and the durable contract

`agent_thread_messages.parts` is a **shipped durable JSON schema**. Rust must
round-trip it, byte-compatibly, or every existing thread breaks.

```rust
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum AgentChatPart {
    Text { text: String },
    Reasoning { text: String, started_at: Option<i64>, last_delta_at: Option<i64> },
    /// `type` is `tool-<name>` or `dynamic-tool` on the wire — a custom
    /// (de)serializer, because the discriminant carries the tool name.
    Tool(ToolPart),
    #[serde(rename = "data-subagent")] Subagent { data: SubagentSnapshot },
    #[serde(rename = "data-pi-message")] ProviderMessage(ProviderMessageMeta),
    #[serde(rename = "step-start")] StepStart,
}
```

`data-pi-message` keeps its wire name even though Pi is gone: renaming a
discriminant in a durable column to match an implementation detail is how you
get a migration for nothing. (The field's *doc comment* at
`src-tauri/src/models/agent_threads.rs:38` still claims these are "the AI SDK
`UIMessage.parts` array verbatim" — stale twice over. Fix it while here.)

**One fold.** `transcript::apply(&mut Transcript, &TurnEvent)` is the single
canonical reducer, in `luma_lib`, and both hosts call it. GPUI does not write
its own; the Tauri webview's `messages.ts` fold is retired against it.

The inverse — `Transcript → Vec<ModelMessage>` for rehydration, splitting one
assistant row back into per-step messages at `StepStart` and rebuilding tool
results through each tool's `ToolOutcome` — is `transcript::to_model_messages`.
This is the highest-fidelity requirement in the port and gets a golden test
against real persisted threads (§5).

### 2.5 The turn protocol and the authored-turn invariant

The ordering from `create-agent-chat.ts:1291-1380` ports as-is. What does
*not* port as-is is the invariant's two open violations
(`project_authored_turn_invariant`), because they are a design bug and this is
the rewrite:

1. **Steering mid-turn.** Pi injects a user message, producing two assistant
   messages; only the last is prepared. **Fix:** call `prepare_turn` when an
   assistant message *opens*, not once per user prompt. Preparation is cheap
   and it is the natural pairing — one preparation per assistant row, which is
   literally what the trigger checks.
2. **Subagent milestone rows.** `flushSubagentSnapshots` persists assistant
   rows with a single `data-subagent` part that nothing prepares. **Fix:** they
   do not go in `agent_thread_messages` at all. A subagent milestone is live UI
   state, not transcript — it belongs on the `TurnEvent` stream and in the
   host's entity, and if it needs durability it gets its own table. Today both
   the persist path and the read path already special-case "every part is
   `data-subagent`" (`messages.ts:430`, `create-agent-chat.ts:1355`), which is
   the codebase telling us these rows are not messages.

With both fixed the trigger needs no exemption clause and the invariant holds
by construction rather than by care.

### 2.6 Cancellation

`TurnStream`'s `Drop` aborts. One rule: dropping the stream cancels the turn,
including a `cancel_python_cell` on any in-flight cell. There is no separate
`cancel()` a caller could forget to pair with a `turn()`.

### 2.7 Retiring the TypeScript stack

The port is only done when the TS stack is deleted, not when it is bypassed.
Sequence:

1. Rust loop ships behind `dispatch`-adjacent commands + `TurnStream`, GPUI is
   its only consumer.
2. Tauri webview switches `createAgentChat` to drive the Rust loop through the
   `Events` adapter, keeping `conversation.tsx` as the renderer.
3. Delete `src/shared/lib/agent/**` except the thin thread/authored clients, and
   `create-agent-chat.ts`. Drop `@earendil-works/pi-*`.

Step 2 is the one that must not be skipped — leaving two loops running is
strictly worse than either candidate above.

---

## 3. UI design (`gpui/crates/chat`, `gpui/crates/md`)

### 3.1 Crates

| crate | contents | notes |
| --- | --- | --- |
| `luma-md` | ported comet `parser.rs`, `mend.rs`, `veil.rs`, `render.rs`, `selection.rs` | MIT notice + per-file header. `render.rs` drops the three `zeron_syntax` call sites (`:22`, `:1218`, `:1240`) behind a `Highlighter` trait with a no-op default — highlighting is pure paint by design, so removing it changes no layout. |
| `luma-chat` | entities, elements, the comet-derived `theme` + `motion` | Depends on `luma-md`, `luma-lib`, `gpui`, `gpui-component`. Does **not** depend on `luma-ui`. |
| `luma-app` | additive: a `chat` field on `Luma`, keymap rows | Owned by this workstream. |

`motion.rs` (943 lines) is lifted near-verbatim into `luma-chat::motion` —
the exact CSS `cubic-bezier` evaluator, the catalog, and critically the shared
30 fps `PulseClock`, which is what stopped one mounted spinner costing 36% CPU
at 120 Hz. Only `speed_scale()`'s env var name is renamed.

`theme.rs` is lifted as *mechanism*, not palette: the `grey`/`neutral`/`oklch`/
`mix` constructors and the context-free `ink`/`hairline`/`wash`/`scrim` helpers
with their `CURRENT_APPEARANCE` mirror and `THEME_GENERATION` counter. The
counter is not optional if we lift `render.rs` — its `TextRun` cache bakes
resolved `Hsla`.

Not lifted: `frost.rs` and `edge_fade.rs` (fork-only APIs), `transcript.rs`
and `composer.rs` (entangled with Zeron's doc model — reimplement against our
data, but port the *numbers*), `zeron-syntax` (deferred).

### 3.2 Entity model

```
Luma  (luma-app)
 └ chat: Option<Entity<AgentChat>>          panel, orthogonal to Screen

AgentChat                                   one thread, one composer
 ├ scope: ThreadScope                        derived from the current Screen
 ├ threads: Vec<AgentThread>                 the picker's list
 ├ transcript: Transcript                    luma_lib's type, verbatim
 ├ rows: Vec<Row>                            render state per transcript row
 ├ list: ListState                           gpui virtualized list
 ├ composer: Entity<InputState>              gpui-component's editor
 ├ turn: TurnState                           Idle | Streaming(Task<()>) | Failed
 └ subagents: Vec<SubagentSnapshot>          live, never persisted (§2.5)

Row                                         one message's render state
 ├ parser: IncrementalParser                 per text part
 ├ tree: BlockTree
 ├ veil: RowVeil                             per-chunk fade spans
 └ cache: RenderCache
```

`Transcript` is `luma_lib`'s type held directly — not mirrored. The host adds
*render* state beside it (`rows`), never a second copy of the content. A
mirrored transcript is the classic two-sources-of-truth bug and the fold
already lives one crate down.

The panel is **orthogonal to `Screen`**, not a variant of it: chat is open
*over* the track editor or the graph, and its `ThreadScope` is derived from
whichever screen is up (`track_copilot`/`subject=track` vs
`pattern_graph`/`subject=pattern`+`implementation_id`). That derivation is one
function, `ThreadScope::for_screen(&Screen)`, so the "`pattern_graph` requires
`implementationId`, `track_copilot` forbids it" rule is stated once.

### 3.3 Streaming path

```
TurnStream ──cx.spawn──▶ AgentChat::on_event
                           ├ luma_lib::transcript::apply(&mut self.transcript, ev)
                           ├ row.parser.append(delta)      O(delta + last block)
                           ├ row.veil.chunk(range)         paint-only fade
                           ├ self.list.remeasure_items(last..last+1)
                           └ cx.notify()
```

Four rules, all from comet, all load-bearing:

1. **Remeasure exactly one row per delta.** `remeasure_items(last..last+1)`.
   Remeasuring the list is the frame budget.
2. **The veil is paint, never layout.** Alpha multiplies into `TextRun`
   colors; cosmic-text's `Attrs::compatible` ignores color, so a color-only
   run split cannot change wrapping. Byte-identical layout.
3. **`mend` the display parse only.** Hanging `**` / `[link](` are auto-closed
   for display so the real closing marker never reflows painted text; the
   canonical parse settles honestly at message end.
4. **Analytic fold heights, never measured.** Tool chips and collapsed
   sections declare their height; measuring them makes every collapse a
   relayout.

Stick-to-bottom uses comet's spring numbers verbatim: damping 0.7, stiffness
0.05, mass 1.25, 8-frame catchup cap, 0.12 growth EMA, 32 px chase lead.

### 3.4 Markdown: port vs. minimal own

**Port.** A minimal renderer is a fake choice: streaming markdown needs
incremental block reparse, hanging-marker mending, and a per-chunk fade that
does not reflow. That is `parser.rs` + `mend.rs` + `veil.rs` — 2,100 lines of
solved problems whose *hard parts are the non-obvious invariants*, not the
code. Writing our own means rediscovering them. The license permits the lift,
and the only edit needed is stubbing three highlighter call sites.

### 3.5 Vibrancy and the sliding panel

**Vibrancy.** Stock gpui at our pin has `WindowBackgroundAppearance` and
`Window::set_background_appearance` (`crates/gpui/src/window.rs:2596`). It does
**not** have `paint_backdrop_blur` — so window-level blur works, per-element
frosted cards do not.

The chat is a panel inside Luma's window, and Luma's window is an opaque
brutalist plane. Making the whole window `Blurred` for the sake of one panel
would put vibrancy behind the venue grid, which is not the app we have. So:

- v1: the panel paints comet's own dark planes (`grey(6)` content, `grey(13)`
  shell) with alpha-tinted overlays. No real backdrop blur; comet's own light
  mode already ships this fallback (`surface_overlay @ 85%`).
- The `set_background_appearance` call lives in exactly one function,
  `chat::appearance::apply`, so turning the window translucent later is one
  line rather than an audit. That function carries the macOS gotcha comet
  documents at `appearance.rs:232`: gpui's macOS backend tears out the
  `NSVisualEffectView` whenever the value is not `Blurred`, so vibrancy must
  be **re-applied after every theme swap**.
- Deferred alternative, recorded so it isn't rediscovered: give chat its own
  window, which can be `Blurred` on its own terms. Rejected for v1 because a
  detached conversation window is a different product decision, not a styling
  one.

**Scroll-edge fade.** `edge_fade.rs` needs fork-only `EdgeFade`. Over an
opaque panel a painted gradient overlay is equivalent and trivial. Ship that.
It only genuinely fails over real vibrancy, which v1 does not have.

**Sliding panel.** Lift comet's `WidthTween` / `eval_tween` / `pane_container`
trio (`shell.rs:479-493`, `:2853-2884`, ~60 lines). The mechanism is a
manually-driven tween on the *container* width with `overflow_hidden` clipping
a **fixed-width inner**, so the panel's content never reflows mid-transition.
This is the same lesson as `project_graph_editor_perf`: layout is the enemy,
animate geometry that clips rather than geometry that measures.

Popover/menu close uses comet's two-phase state machine — `begin_close` →
100 ms `MENU_OUT` → `finish_close`, under a *fresh* element id so the entrance
timeline does not resume mid-exit.

### 3.6 Composer and keymap

The composer is `gpui_component::input`'s editor (`InputState` / `TextInput`),
already a workspace dependency — a real multi-line field with caret, selection
and IME. `luma_ui::luma_input` is a resting-state-only control and is not a
candidate; the chat is the first place in the GPUI app that needs real text
entry.

Keymap additions (`gpui/crates/app/src/keymap.rs`, additive):

```rust
pub const AGENT_CHAT: &str = "AgentChat";     // the panel root

actions!(luma, [ToggleAgentChat]);

// Not `secondary-l`: that is the track editor's loop region.
KeyBinding::new("secondary-shift-l", ToggleAgentChat, Some(context::ROOT)),
```

**The composer element declares `context::TEXT_INPUT`.** This is not optional:
`space` (`PlayPause`) and `escape` (`Back`) are both bound with
`&& !TEXT_INPUT`, gpui matches bindings *before* delivering key events, and
without the context every space typed into the composer would toggle transport
and every escape would leave the screen. Same rule the tracks search field
follows (`tracks.rs:402`).

Consequently `escape` inside the composer is handled in `on_key_down` — cancel
the streaming turn if one is running, otherwise close the panel — rather than
by a binding. One rule, stated once, matching the existing precedent.

### 3.7 Harness instrumentation

The chat elements carry `agent_node` annotations so `gpui-agent` can see them:

- streamed assistant markdown → `Role::Text` with the rendered plain text
- the composer → `Role::Input`
- send / stop / thread-picker → `Role::Button`, `Role::Select`
- **a tool-call chip → a new `Role::Chip`** in `luma_ui::node`, labelled
  `"<verb> <summary>"` from the ported `tool-verbs` phrasing.

`Role::Chip` is the one addition to the role vocabulary. A tool chip is not a
button (not pressable at rest), not text (it has state), and not a row. Adding
a role is cheaper than asserting on a `Role::Text` whose label is a phrasing
detail.

---

## 4. The interface between runtime and UI, exactly

Everything the UI knows about the agent is on this page.

### Types (`luma_lib::agent`, re-exported from `luma_lib`)

```rust
pub struct ThreadScope {           // identity of a conversation
    pub principal_id: Option<String>,
    pub agent_kind: AgentKind,     // TrackCopilot | PatternGraph
    pub subject_kind: SubjectKind, // Track | Pattern
    pub subject_id: String,
    pub implementation_id: Option<String>,  // required for PatternGraph, forbidden for TrackCopilot
    pub venue_id: Option<String>,
    pub score_id: Option<String>,
}

pub struct Transcript { pub messages: Vec<AgentChatMessage> }
pub struct AgentChatMessage { pub id: String, pub role: Role, pub parts: Vec<AgentChatPart> }

#[non_exhaustive]
pub enum TurnEvent {
    MessageStarted   { id: String, role: Role },
    StepStarted,
    TextDelta        { text: String },
    ReasoningDelta   { text: String },
    ToolCallStarted  { call_id: String, name: String, input: Value },
    ToolCallEnded    { call_id: String, output: ToolOutcome },
    Subagent         { snapshot: SubagentSnapshot },   // live only, never persisted
    DocumentChanged  { revision: String },             // the editor should re-read
    PreviewSelection { expression: Option<String> },   // ephemeral editor state
    MessageEnded     { stop_reason: StopReason, usage: Usage },
    TurnEnded        { outcome: TurnOutcome },         // Completed | Cancelled | Failed(String)
}

pub struct TurnStream;             // Stream<Item = TurnEvent>; Drop cancels the turn
```

### Calls the UI makes

```rust
impl AgentService {
    pub async fn resolve_thread(&self, scope: &ThreadScope) -> Result<AgentThreadDetail, AgentError>;
    pub async fn list_threads(&self, scope: &ThreadScope) -> Result<Vec<AgentThread>, AgentError>;
    pub fn turn(&self, thread_id: &str, prompt: UserPrompt) -> TurnStream;
    pub fn steer(&self, thread_id: &str, message: String);   // into a running turn
    pub async fn rename(&self, thread_id: &str, title: Option<String>) -> Result<AgentThread, AgentError>;
    pub async fn delete(&self, thread_id: &str) -> Result<(), AgentError>;
}
```

`AgentService` hangs off `AppServices` and is reachable from `Library` — the
GPUI app's one door to data stays one door. Thread CRUD keeps going through
`dispatch` where it already does; only `turn`/`steer` need the typed path,
because only they stream.

### The fold, called by both hosts

```rust
pub fn apply(transcript: &mut Transcript, event: &TurnEvent) -> Applied;
/// Which row changed, so a host can remeasure exactly that one.
pub struct Applied { pub row: usize, pub appended: Option<Range<usize>> }
```

`appended` is the character range the veil fades. It is the whole reason
`TurnEvent` is a delta vocabulary rather than a snapshot one.

### Commands added to the seam

Only what a webview needs and a typed caller does not:

| command | why |
| --- | --- |
| `agent_turn_start(threadId, prompt) -> turnId` | webview cannot hold a `TurnStream` |
| `agent_turn_cancel(threadId)` | ditto — GPUI drops the stream instead |
| `agent_steer(threadId, message)` | shared |

Plus one event name on the existing bus, emitted by the Tauri adapter only:
`"agent-turn"`, payload `{ threadId, turnId, event: TurnEvent }`.

---

## 5. Exit gates

1. **Turn protocol, no network** — `#[tokio::test]` in `luma_lib`: `AgentService`
   over a `ScriptedModel` and a temp database runs a full turn with one tool
   call. Asserts the transcript equals a golden, that `authored_turn_preparations`
   has exactly one row per assistant message, and that the insert clears the
   `assistant_message_requires_prepared_authored_turn` trigger.
2. **Steering does not violate the invariant** — same shape, with a steer
   injected mid-turn. This is the regression test for §2.5(1) and it must fail
   against the current TS behaviour.
3. **Durable round-trip** — fixture threads captured from a real
   TS-written database: `parts` JSON → `Vec<AgentChatPart>` → JSON compares
   equal, and `to_model_messages` reproduces the recorded rehydration.
4. **Markdown incrementality** — `luma-md` property test: feeding a document
   in N random chunk splits yields the same final `BlockTree` as `parse_full`,
   and no chunk reparses more than the last stable block.
5. **GPUI streaming render** — a `gpui-agent` script against `Luma` built on a
   `Library` whose `AgentService` is scripted. Opens the chat, types into the
   composer, sends; asserts (a) a `Role::Text` node whose label contains the
   streamed prose *before* the turn ends, (b) a `Role::Chip` node for the tool
   call with the expected verb, (c) two snapshots during streaming differ, so
   we are testing streaming and not a final paint, (d) `space` typed in the
   composer inserts a space and does not toggle transport.
6. **Live smoke, conditional** — `#[ignore]` unless `LUMA_OPENROUTER_API_KEY`
   is set: one real turn against `moonshotai/kimi-k3-fast`, asserting only that
   deltas arrive and the turn completes. Never in CI's default set.
7. `cargo doc --no-deps -p luma-chat -p luma-md` clean, per `CLAUDE.md`.

---

## 6. Build plan

### Runtime (`luma_lib::agent`) — no GPUI, no window

| # | work | gate |
| --- | --- | --- |
| R1 | `transcript.rs`: `AgentChatPart` + serde matching the durable column | gate 3 |
| R2 | `model/`: `ModelClient`, `ModelEvent`, the single `ModelId` table, `ScriptedModel` | — |
| R3 | `model/openrouter.rs` — openai-completions SSE | gate 6 |
| R4 | `tools/`: `Tool` trait, registry, `schemars` schemas; `python` + `skill` first (the whole track-agent surface) | — |
| R5 | `AgentService::turn` + `TurnStream` + the turn ordering; the two invariant fixes | gates 1, 2 |
| R6 | `transcript::apply` + `to_model_messages` | gates 1, 3 |
| R7 | prompts + skills as `include_str!`, byte-stable | — |
| R8 | key resolution (env → settings), `NotConfigured` naming the provider | — |
| R9 | graph tools, `ask_venue`, backend-side `preview` PNG | — |
| R10 | subagents: `SubagentManager`, frontmatter `AgentLoader`, workspace supervisor | — |
| R11 | `model/anthropic.rs` | — |
| R12 | Tauri `Events` adapter + the three seam commands; retire the TS loop | — |

R1–R6 is the vertical slice: a track-agent turn with `python`, end to end,
tested, before any UI exists.

### UI (`gpui/crates/md`, `gpui/crates/chat`, additive `luma-app`)

| # | work | gate |
| --- | --- | --- |
| U1 | `luma-md`: lift `parser`/`mend`/`veil`, license notices, `Highlighter` stub | gate 4 |
| U2 | `luma-md`: lift `render` + `selection`; lift `theme` mechanism + `THEME_GENERATION` | — |
| U3 | `luma-chat::motion`: lift `motion.rs` incl. `PulseClock` | — |
| U4 | `luma-chat::theme`: comet palette, isolated from `luma-ui::ladder` | — |
| U5 | `AgentChat` entity + `ListState` transcript, static (no streaming) | — |
| U6 | streaming: `TurnStream` fold, per-delta `remeasure_items`, veil | gate 5 |
| U7 | composer on `gpui_component::input`, `TEXT_INPUT` context, escape-cancels | gate 5(d) |
| U8 | tool chips + `Role::Chip` in `luma_ui::node` + ported verb phrasing | gate 5(b) |
| U9 | sliding panel: `WidthTween` + `pane_container`; `chat::appearance::apply` | — |
| U10 | thread picker, history, restore | — |
| U11 | subagent pane | — |

U1–U3 can start immediately and in parallel with R1 — they touch nothing the
runtime owns. U6 is the first point the two halves meet, and it meets them at
`TurnEvent` + `transcript::apply`, which is the only interface between them.

---

## 7. Smells this port must not carry forward

Flagged per `CLAUDE.md`; each is fixed by a row above, not deferred.

- Four independent model-id sources (`AGENT_MODELS`, hardcoded
  `x-ai/grok-4.5`, `VENUE_EXPERT_MODEL`, unvalidated subagent override) → one
  `ModelId` table (R2).
- `notConfiguredMessage` naming OpenRouter regardless of the selected
  provider (R8).
- API keys in `localStorage`, which is why headless has to read WebKit's
  sqlite file (R8).
- `shared/lib/agent/openrouter.ts` and `ask-venue-tool.ts` importing *upward*
  from `shared/` into `features/track-editor/` for key + model config —
  inverted layering, deleted by the port.
- `src-tauri/src/models/agent_threads.rs:38` documenting `parts` as "the AI
  SDK `UIMessage.parts` array verbatim" — a stale contract comment on a
  durable schema, and stale twice (the SDK was replaced by Pi, and Pi by
  this) (R1).
- `resolveThread`'s "newest matching thread wins, ambiently" — carried
  forward deliberately for now, but `ThreadScope::for_screen` is where a
  better rule would go, and it should get one before the picker ships (U10).
- `gateway-fetch.ts` monkey-patching `globalThis.fetch` — deleted, not
  ported (R3).

---

## 8. Runtime as built (R1–R8, R11, R12-partial)

The runtime half is implemented in `src-tauri/src/agent/**`. Where the code
differs from §1–§4 above, the reason is recorded here; the sections above are
otherwise unchanged and still the contract.

### Deviations, with reasons

1. **Anthropic first, OpenRouter alongside.** R11 was pulled ahead of R3 —
   `claude-opus-5` routes direct, and Kimi/Grok route over OpenRouter, from the
   same `MODELS` table. `ModelSpec` therefore carries a *per-provider wire id*
   (`anthropic`, `openrouter`) rather than one provider field: provider and
   model are separate axes, and a model can be reachable through both.
   `ModelId::route(preferred)` falls back to whichever provider actually serves
   the model instead of minting an id the provider would reject.

2. **`TurnStream` is consumer-driven, not a spawned task.** The turn's future is
   *inside* the stream and is polled by whoever reads it. Cancellation is then
   Rust's ordinary drop — no abort guard, no token, no registry — and the
   Python cell's interrupt hangs off a `Drop` guard in the tool. The webview
   adapter (`agent::host`) is the only thing that spawns, because only it must
   outlive its caller; it keeps a `TurnSteer` handle since it no longer holds
   the stream.

3. **`AgentService` owns `Arc<AppServices>`.** A turn outlives the command that
   starts it, so it cannot borrow a command body's `&AppServices`.
   `AppServices::into_shared()` is the host-side installer; the Tauri adapter
   now manages `Arc<AppServices>` (one line in the `commands!` macro). Tool
   bodies still take `&AppServices`, as §2.3 says.

4. **`TurnEvent` gained `StepEnded`, and `MessageEnded` gained an `id`.** A
   turn's assistant row holds several steps, and the `data-pi-message` part is
   per *step*; `MessageEnded` marks the durable close of the row. Collapsing
   the two would have made one event mean two things.

5. **`ToolCallEnded { output: ToolResult }`, not `ToolOutcome`.** The transcript
   stores what the tool persisted; the model-facing rendering is derived from
   it by `Tool::stored_output`, so rehydrating an old thread reproduces exactly
   what the model saw. Correspondingly `Tool::call` returns the stored value and
   `Tool::schema` returns an owned `Value` (from a `schemars` derive on the
   argument struct) rather than `&'static Value`.

6. **`Applied` carries `part` as well as `row`.** A row holds several
   independently growing text parts; a fade span without a part index is
   ambiguous.

7. **No `AgentChatPart::Subagent`.** Live milestones are `TurnEvent::Subagent`
   only (§2.5), and legacy `data-subagent` rows round-trip through
   `AgentChatPart::Unknown`, which preserves *any* unknown part verbatim rather
   than truncating a transcript a future build wrote.

8. **`prepare_turn` runs at the assistant row's close, not its open.** §2.5's
   pairing (one preparation per assistant row) is what fixes the steering
   violation, and it holds either way — but preparation snapshots the authored
   document the turn produced, so taking it before the tools run would record
   the wrong state. Both invariant fixes are covered by tests
   (`agent::tests::steering_mid_turn_prepares_every_assistant_row`).

9. **The track prompt's dynamic block is gone.** `build-context.ts` interpolated
   track name/BPM/bars *near the top* of the prompt, which invalidates the
   prompt-cache prefix on every thread. Those facts are already in `luma.track`;
   `prompts/track.md` is byte-stable.

10. **`AgentService::with_tools`** is the one injection seam: a test substitutes
    a scripted tool, and a subagent will substitute a differently-bound context
    through the same path.

### Not built yet

Skills, subagents (`SubagentManager`, frontmatter loader), the graph tools,
`ask_venue`, backend-side `preview` (R9, R10), and the retirement of the
TypeScript loop (R12's second half). `TurnEvent::Subagent` and
`TurnEvent::PreviewSelection` exist and fold correctly; nothing emits them yet.
`settings::AGENT_MODELS` remains a second spelling of the model table until the
TypeScript loop is deleted — a test now fails if the two drift.

---

## 9. UI as built (U1–U9)

The surface is implemented in `gpui/crates/md` (`luma-md`) and
`gpui/crates/chat` (`luma-chat`), with additive rows in `luma-app`. Where the
code differs from §3, the reason is recorded here; §3 is otherwise unchanged
and still the contract.

### Deviations, with reasons

1. **`theme` lives in `luma-md`, not `luma-chat`.** §3.1 splits the mechanism
   from the palette across the two crates, which cannot work: `render.rs`
   *paints*, so it needs tokens, and a renderer reaching up into the crate
   above it is the inverted layering this port exists to remove. One theme
   module, in the lower crate, re-exported by `luma_chat::theme`.

2. **Dark only.** zeron's light appearance is a designed second palette, and
   Luma has no light mode to design against; shipping the unreachable half
   would be a palette nobody paints. `Theme::dark` is the only constructor and
   the `ink`/`hairline`/`wash`/`scrim` helpers resolve directly.
   `theme_generation` survives anyway — `render.rs`'s `TextRun` cache bakes a
   resolved `Hsla`, so a second appearance has to be a counter bump and not a
   hunt through cache keys.

3. **`Highlighter` returns colors, not token kinds.** §3.1 asks for a trait
   with a no-op default; it takes a `HighlightSpan { range, color }` rather
   than a `HighlightKind`, because highlighting is pure paint and a color is
   the whole of what the renderer needs. A kind would have dragged an enum and
   a syntax palette across the seam with no reader.

4. **The code block's copy button is not ported.** It was the only reason
   `render.rs` reached into an icon set and a motion kit, and nothing here has
   a clipboard affordance yet. `RenderOptions::copy` is gone with it.

5. **`RenderCache` grew `invalidate_from`.** The cache is keyed by *position*,
   so an entry whose block was reparsed is stale and nothing inside `luma-md`
   can tell — only `IncrementalParser::stable_prefix_blocks` knows. Without it
   a streamed paragraph paints as it stood when its block was first flattened,
   which is exactly what the first capture showed. `Row::sync` calls it beside
   the reparse.

6. **Stick-to-bottom is `ListAlignment::Bottom`, not the spring.** §3.3's
   spring numbers are a refinement over a list that is already bottom-pinned;
   the spring is what makes a *fast* stream read smoothly, and it wants a
   scroll handler and a per-frame integration. Recorded, not built.

7. **The panel's scope covers `pattern_graph` only.** `crate::agent::scope_for`
   is the one function §3.2 asks for, but the track editor's state does not
   publish the track and venue a `track_copilot` scope needs, and widening it
   is that screen's change to make. The arm is three lines when it does.

8. **`luma_ui::TEXT_INPUT`.** The key-context name moved down to `luma-ui`:
   it is half of a pair (a binding predicate in `keymap.rs`, a `key_context`
   on the composer in another crate), and a name spelled differently in the
   two places is a binding that silently never fires.

9. **Turn events cross a channel, not a stream.** `AgentService::turn` hands
   back a stream whose *work* is polled by its reader, and that work is `sqlx`
   — which needs the Tokio reactor `Library` owns and gpui does not have. So
   `luma_chat::Agent` drains it on that runtime and forwards. The
   drop-cancels contract survives: dropping `Turn` closes the channel, the
   forwarder's send fails, and it drops the `TurnStream`.

10. **`Library` grew two injection seams**, `set_agent_model` and
    `set_agent_tools`, both `None` in the shipped app. They are how the
    harness drives a turn without a network — the same `with_model` /
    `with_tools` the runtime already exposes, reached through the one door
    this host has to data.

### Exit gates as met

- Gate 4 (markdown incrementality) is the ported parity suite:
  `cargo test -p luma-md` → 68 passed, including the streamed-corpora and
  display-tree property tests.
- Gate 5 (GPUI streaming render) is `gpui/crates/agent/tests/agent_chat.rs`:
  a scripted model and a slow scripted tool over a real thread and a real
  database, asserting the streamed prose, the tool chip in both tenses, that
  the transcript grows across the turn, and that a space typed into the
  composer stays a space.
- The reference plates are `harness/gauntlet-chat/gpui-chat-{idle,streaming,
  finished}.png`, written by the `#[ignore]`d generator
  `gpui/crates/agent/tests/gauntlet_chat.rs`.

### Not built

The thread picker and history (U10), the subagent pane (U11), text selection
across rows (ported but unwired), reasoning as a collapsible fold, window
vibrancy (`chat::appearance::apply` — the panel paints comet's opaque planes,
which is what §3.5 calls v1), and the scroll-edge fade.
