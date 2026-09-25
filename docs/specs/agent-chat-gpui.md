# Agent chat

Status: built. Code: `backend/src/agent/`, `gpui/crates/md` (`luma-md`),
`gpui/crates/chat` (`luma-chat`), and in `gpui/crates/app/src/`: `agent.rs`,
`chat_history.rs`, `subagents.rs`.

Related: `docs/design/skills.md`, `docs/design/subagents.md`,
`docs/design/prompt-caching.md`, `docs/design/agent-code-execution.md`.
Code cites the section numbers of this document.

## 0. Style and license

The chat is a chrome surface. Its style contract is
`docs/specs/comet-shell.md` §9. `harness/gauntlet-chat/style-spec.md` records
the visual bar.

The markdown renderer and parts of the chat are ported from zeron/comet (MIT,
© 2026 Wing). The license is `gpui/crates/md/THIRD_PARTY/zeron-MIT.txt`. Every
ported file starts with
`//! Ported from zeron (MIT, © 2026 Wing) — <original path>`.

## 1. Where the agent loop lives

The loop is `luma_lib::agent` (`backend/src/agent/`). It runs without a window,
so `luma-agent` and `luma-mcp` use the same loop as the app.

`AgentService::turn(thread_id, prompt)` returns a `TurnStream`. A host reads
typed `TurnEvent`s from it. Dropping the stream cancels the turn.

Turn events do not go through the dispatch `EventSink`. That bus is a
string-keyed, app-wide broadcast of JSON values. Turn events are per turn,
ordered, frequent and typed.

The GPUI host drains the stream on the Tokio runtime that `Library` owns,
because the turn's work uses `sqlx`. It forwards the events to the entity over
a channel. Dropping the host's `Turn` closes the channel, and that drops the
`TurnStream`.

## 2. Runtime

### 2.1 Models and engines

`ModelClient` is the one streaming trait. `model/mod.rs` holds the one model
table (`claude-opus-5`, `kimi-k3-fast`, `grok-4.5`). Each model spec carries a
wire id per provider. `ModelId::route` picks a provider that serves the model.

| provider | transport |
|---|---|
| `VercelAiGateway` (default) | `model/anthropic.rs` against the gateway |
| `Anthropic` | `model/anthropic.rs` |
| `OpenRouter` | `model/openrouter.rs` |

A thread also names an engine (`agent/engine/`): `Api` runs the loop against
the providers above; `Codex` and `Claude` run the installed CLIs with their own
accounts and give them Luma's tools.

### 2.2 Keys

`model::api_key(provider)` reads `LUMA_ANTHROPIC_API_KEY`,
`LUMA_OPENROUTER_API_KEY` or `LUMA_AI_GATEWAY_API_KEY` first, then the settings
table. A missing key is `ModelError::NotConfigured`.

### 2.3 Tools

A `Tool` is bound to a `ToolContext`, never to a host. The same registry
function builds the tools for a parent and for a subagent.
`tools::registry_for_context` returns `python`, `skill`, and `subagent` when the
thread edits a score. Each description is an `include_str!` file in
`backend/src/agent/prompts/`, so the bytes stay stable for prompt caching.

### 2.4 The transcript

`agent_thread_messages.parts` is a durable JSON schema. `AgentChatPart` is:

| variant | wire `type` |
|---|---|
| `Text` | `text` |
| `Reasoning` | `reasoning` |
| `Tool` | `tool-<name>` or `dynamic-tool` |
| `ProviderMessage` | `data-pi-message` (one per model step, with usage) |
| `StepStart` | `step-start` |
| `Unknown` | any other part, kept verbatim |

`transcript::apply(&mut Transcript, &TurnEvent) -> Applied` is the one fold.
Every host calls it. `Applied { row, part, appended }` names the row and part
that grew and the appended byte range, so a host remeasures one row and fades
only the new text. `transcript::to_model_messages` rebuilds the model request
from stored rows.

### 2.5 Live state is not transcript

Some events describe the moment and are never stored:

- `TurnEvent::Subagent` carries a `SubagentSnapshot`.
- `TurnEvent::DocumentChanged` tells an editor to read its document again.
- `TurnEvent::PreviewSelection` carries ephemeral editor state.

The fold returns `Applied { row: None, .. }` for them. Anything durable about a
subagent is already a row in its own thread.

A message sent while a turn runs is steering. `TurnStream::steering()` returns
a `TurnSteer` handle, and `TurnSteer::steer` adds the message to the running
turn.

### 2.6 Cancellation

Dropping the `TurnStream` cancels the turn. A running Python cell is
interrupted by a `Drop` guard in the tool. A subagent's turn runs inside its
parent's tool call, so it is cancelled with the parent.

## 3. UI

### 3.1 Crates

| crate | contents |
|---|---|
| `luma-md` | ported zeron `parser`, `mend`, `veil`, `render`, `selection`, plus `syntax` and `theme` |
| `luma-chat` | the `AgentChat` entity and its elements; depends on `luma-md` and `luma-ui` |
| `luma-app` | `agent.rs` (scope), `chat_history.rs` (the history picker), `subagents.rs` (the subagent dialog) |

`luma-md` has only a dark theme.

### 3.2 Entity and scope

`AgentChat` holds `luma_lib`'s `Transcript` directly, plus render state per row.
It never keeps a second copy of the content. It also holds the live
`SubagentSnapshot`s.

`scope_for` (`gpui/crates/app/src/agent.rs`) sets the working context for the
next turn. It uses the open track editor (the active tab, else the last track
tab), then the graph editor's score, then the venue in the sidebar.

### 3.3 Streaming rules

1. Remeasure exactly one row per delta.
2. The veil is paint, never layout. Alpha multiplies into text colors, so
   wrapping does not change.
3. `mend` changes only the display parse. Open markers such as `**` are closed
   for display; the real parse settles at message end.
4. Tool chips and folds declare their height. They are not measured.

The transcript sticks to the bottom with a spring (`transcript::StickSpring`).

### 3.4 Surfaces

- `chip.rs`: tool chips, including the delegation chip. Each chip has the
  `Role::Chip` automation role from `luma_ui::node`.
- `python_cell.rs`: the Python cell view.
- `composer.rs`, `send_motion.rs`, `working.rs`: the composer and its motion.
- `model_picker.rs`: engine and model per thread.
- `usage.rs`: the usage card, including cache reads and writes.
- `subagents.rs`: the pill that counts running subagents.

The composer declares the `TEXT_INPUT` key context, so `space` and `escape`
bindings in `keymap.rs` do not fire while a person types.

## 4. Interface between runtime and UI

`TurnEvent`: `MessageStarted`, `StepStarted`, `TextDelta`, `ReasoningDelta`,
`FinalAnswer`, `ToolCallStarted`, `ToolCallEnded`, `StepEnded`, `Subagent`,
`DocumentChanged`, `PreviewSelection`, `MessageEnded`, `TurnEnded`.

`AgentService` calls a host makes: `history`, `open_thread`, `new_thread`,
`resolve_thread`, `list_threads`, `turn`, `models`, `set_thread_engine`,
`set_thread_selection`. `TurnStream::steering` returns the `TurnSteer` handle.

`AgentService::with_model` and `with_tools` inject a scripted model or tool.
`Library::set_agent_model` and `set_agent_tools` reach them from the app. The
shipped app sets neither.

## 5. Tests

- `backend/src/agent/tests.rs`: turns over a scripted model and a temporary
  database. Live tests are `#[ignore]` and need `LUMA_AI_GATEWAY_API_KEY`.
- `gpui/crates/agent/tests/js/headless/chat*.test.js`: headless chat tests
  over a scripted model (the fixture's `model` and `tools` options). Run
  with `gpui/test`.
- `gpui/crates/agent/tests/app_pixel/gauntlet_chat.rs`: the reference plates
  `harness/gauntlet-chat/gpui-chat-{idle,streaming,finished}.png`.
- `cargo test -p luma-md`: markdown parity and incremental-parse tests.

## 6. Not built

- Window vibrancy. The panel paints opaque planes; nothing calls
  `set_background_appearance`.
- Dedicated graph tools and an `ask_venue` tool. Graph editing goes through
  Python (`edit.graph`).
