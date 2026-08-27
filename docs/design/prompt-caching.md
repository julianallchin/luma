# Prompt caching

**Status:** design, phase 1 (report). No code has been written against this yet.

Today Luma writes **zero** `cache_control` breakpoints. `grep -rn 'cache_control' src-tauri/src src gpui/crates` returns nothing. Every turn re-reads the whole conversation at full input price on Anthropic and the Vercel gateway. The *reading* half is already built — `Usage` carries `cache_read_input_tokens` / `cache_creation_input_tokens` and the usage card renders both — so the instrument that proves a fix works is in place before the fix is.

The good news from the audit: our prefix is already byte-stable. Nothing dynamic sits in the system prompt or the tool definitions. This is a "place three markers" job, not a "restructure the prompt" job.

---

## 1. Pi's caching strategy

Source read: `badlogic/pi-mono` @ `1defa151` (2026-08-27), `packages/ai` v0.84.3. Paths below are relative to that repo root.

### The shape of it

There is **no breakpoint state machine**. Pi recomputes the same fixed set of markers from scratch on every request, from a single `cacheRetention` knob. Nothing is persisted, nothing rotates, nothing is removed before being re-added. This is the design decision to copy: Anthropic caches the longest matching prefix regardless of whether the *old* marker is still present, so the message-tail marker advances naturally each turn while the system and tools markers stay pinned. The 4-breakpoint ceiling is respected **by construction**, not by bookkeeping.

### The one knob

`resolveCacheRetention` / `getCacheControl` — `packages/ai/src/api/anthropic-messages.ts:49-73`:

```ts
function getCacheControl(model, cacheRetention?, env?) {
	const retention = resolveCacheRetention(cacheRetention, env);
	if (retention === "none") return { retention };
	const ttl = retention === "long" && getAnthropicCompat(model).supportsLongCacheRetention ? "1h" : undefined;
	return { retention, cacheControl: { type: "ephemeral", ...(ttl && { ttl }) } };
}
```

`CacheRetention` is `"none" | "short" | "long"` (`packages/ai/src/types.ts:108`, `:200-204`), default `"short"`. Precedence: explicit option → `PI_CACHE_RETENTION=long` env → per-model `compat.supportsLongCacheRetention` veto. `"none"` drops every marker *and* the `prompt_cache_key` / session-affinity id (`anthropic-messages.ts:548-549`, `openai-completions.ts:348-349`).

### The three placement sites (Anthropic native)

**System** — `anthropic-messages.ts:1008-1032`. Every system text block gets a marker. One block for an API key; **two** on the OAuth path (a Claude Code identity block plus the real prompt).

**Tools — the last one only** — `anthropic-messages.ts:1355-1361`, inside `convertTools`:

```ts
...(cacheControl && index === tools.length - 1 ? { cache_control: cacheControl } : {}),
```

Gated by `compat.supportsCacheControlOnTools` at the call site (`:1040-1055`). Deferred tools (`defer_loading`) are passed `undefined`, so the marker lands on the last *immediate* tool and the deferred ones are appended after it — deliberately keeping the marked prefix intact when the deferred set changes.

**Messages — the last message, only if it is a user message** — `anthropic-messages.ts:1295-1317`:

```ts
	if (cacheControl && params.length > 0) {
		const lastMessage = params[params.length - 1];
		if (lastMessage.role === "user") {
			const lastBlock = lastMessage.content[lastMessage.content.length - 1];
			if (lastBlock && (lastBlock.type === "text" || lastBlock.type === "image" || lastBlock.type === "tool_result")) {
				lastBlock.cache_control = cacheControl;
			}
		} …
	}
```

Anthropic's shape collapses tool results into a synthetic user message, so in an agentic loop this marker lands on the **last `tool_result` block of the trailing tool-result batch**. If the last entry is an assistant turn, Pi places **no message breakpoint at all** — it just skips.

Budget: 2 system (OAuth) + 1 tool + 1 message = exactly 4. Non-OAuth = 3. No "last N messages", no second-to-last anchor.

### TTL

Default is **5 minutes** — bare `{ type: "ephemeral" }`, no `ttl` field. `ttl: "1h"` only when retention resolves to `"long"` *and* the model's compat allows it. No `extended-cache-ttl-*` beta header is sent; the beta headers assembled at `anthropic-messages.ts:913, 936, 955` carry only interleaved-thinking / eager-tool-streaming / oauth flags.

One consumer choice worth copying: **compaction and summarization requests force `cacheRetention: "none"`** so a one-off fork does not pay a cache write — `packages/coding-agent/src/core/compaction/compaction.ts:586-593`, mirrored at `packages/agent/src/harness/compaction/compaction.ts:113`.

### OpenRouter / OpenAI-compatible

Pi **does** inject `cache_control` for Anthropic models routed through OpenRouter, auto-detected by model-id prefix — `packages/ai/src/api/openai-completions.ts:1621`:

```ts
const cacheControlFormat = provider === "openrouter" && model.id.startsWith("anthropic/") ? "anthropic" : undefined;
```

That is the only auto-detection; anything else opts in via `model.compat.cacheControlFormat`. It is a single-valued union (`types.ts:616`) — there is no `openai-generic` variant, because there is nothing to place for implicit-caching providers.

`applyAnthropicCacheControl` (`openai-completions.ts:1064-1110`) mirrors the native path with one meaningful difference: the message marker scans **backwards** for the last `user | assistant | tool` message rather than requiring the final entry to be a user message, and `addCacheControlToTextContent` (`:1134-1165`) returns `false` on empty content so the scan continues. Max 3 markers here (one system message). TTL variant at `:1053-1062`.

Other providers: OpenAI gets `prompt_cache_key` / `prompt_cache_retention` (`openai-completions.ts:805-810`) and never a marker; Gemini is implicit-only (`google-generative-ai.ts` sets no cache field, reads `cachedContentTokenCount` back); Bedrock uses `cachePoint` blocks on system and last-user-message but **not** on tools (`bedrock-converse-stream.ts:876-880`, `:1086-1096`).

### How Pi keeps the prefix stable

- **Tools are never sorted.** No `.sort(` anywhere in `packages/ai/src/api/`. Stability is the caller's contract; `splitDeferredTools` partitions without reordering.
- **The system prompt carries `cwd` but no date or time.** `packages/coding-agent/src/core/system-prompt.ts:167` appends `Current working directory: ${promptCwd}` — stable for a session. There is no `<env>` block, no "Today's date", no git-branch injection. Project context files and skill listings are baked in once (`system-prompt.ts:52-68, 148-162`).
- **Per-turn dynamic context is never injected into the system prompt.** It arrives as ordinary tool results appended to the message tail — i.e. after the last breakpoint's prefix.

### How Pi reads usage and cost back

Anthropic, at `message_start` (`anthropic-messages.ts:602-609`) with a null-safe re-read at `message_delta` (`:741-762`, explicitly "preserves input_tokens from message_start when proxies omit it"). Note it splits `cacheWrite1h` out of `cacheWrite` via `usage.cache_creation.ephemeral_1h_input_tokens`, so 1h writes can be priced at their own rate.

OpenAI-compat (`openai-completions.ts:1498-1538`) reads `prompt_tokens_details.cached_tokens ?? prompt_cache_hit_tokens ?? cached_tokens` (OpenRouter / DeepSeek / Kimi respectively) and computes `input = max(0, prompt_tokens - cacheRead - cacheWrite)` — subtracting **both**, with a comment warning not to subtract writes from `cached_tokens`.

Cost is one function for every provider — `packages/ai/src/models.ts:878-898` — with tiered long-context rates and the Anthropic 1h premium priced as `rates.input * 2 * longWrite`.

### The piece worth stealing outright

`packages/coding-agent/src/core/cache-stats.ts` is a cache-**miss detector** built purely on usage readback. Per assistant turn it computes `missedTokens = min(prevPromptTokens, promptTokens) - cacheRead`, prices the miss at `(paidPerToken - readPerToken)`, and surfaces it with the idle gap and whether the model changed. Details:

- `NOISE_FLOOR_TOKENS = 1024` — "per-turn misses at or below this are cache breakpoint granularity noise."
- `CACHE_TTL_MS = 5 * 60 * 1000`.
- A sticky `reportedCache` flag distinguishes a total miss from a provider that never reports caching at all.
- Compaction resets `prev`; **model switches are deliberately not exempt** and count as misses.

This is Pi's answer to the fact that its markers can leave a rewound turn's prefix uncovered: rather than place extra anchors, it *measures* the waste. We already have the raw material for it in `RequestUsage`.

---

## 2. Audit: our request assembly

There is exactly **one** production site that builds a `ModelRequest`: `src-tauri/src/agent/turn.rs:165-172`.

```rust
let request = ModelRequest {
    model: setup.model,
    system: setup.kind.system_prompt().to_string(),
    messages: self.model_messages(setup.registry),
    tools: setup.registry.specs(),
    reasoning: setup.reasoning,
    max_tokens: MAX_TOKENS,
};
```

Every field, traced:

| Prefix element | Source | Byte-stable across turns? | Across app restarts? | Notes |
|---|---|---|---|---|
| `system` | `AgentKind::system_prompt()` — `src-tauri/src/agent/mod.rs:90-95`, `include_str!("prompts/track.md")` / `graph.md` | **Yes** | **Yes** (compiled into the binary) | No format string, no interpolation, no date, no venue name, no cwd. This is the single biggest thing we got right by accident. |
| `tools[].name` | `PythonTool::name()` — `tools/python.rs:54-56`, `&'static str` | Yes | Yes | |
| `tools[].description` | `PYTHON_TOOL_DESCRIPTION` — `tools/python.rs:29`, `include_str!("../prompts/python-tool.md")` | **Yes** | **Yes** | The doc comment at `tools/python.rs:23-25` already states the invariant: *"The description is a cached prompt prefix: it must stay byte-stable for a thread's lifetime, so it lives in a file rather than in a format string."* It contains **no catalog** — `luma.catalog()` is named as a callable, not inlined. |
| `tools[].schema` | `schemars::schema_for!(PythonArgs)` — `tools/python.rs:62-66` | Yes | Yes | `serde_json` is built **without** `preserve_order` (`src-tauri/Cargo.toml:67` is a bare `serde_json = "1"`, and `preserve_order` appears nowhere in `Cargo.lock`), so every `Map` is a `BTreeMap` and serializes in sorted key order. Deterministic. |
| tool **order** | `ToolRegistry.tools: Vec` — `tools/mod.rs:88-90`, built by `tools::registry(kind)` at `tools/mod.rs:129-137` | Yes | Yes | Currently one tool, so order is trivially stable. The doc comment at `tools/mod.rs:85-86` already flags ordering as a caching correctness concern. |
| `messages` | `transcript::to_model_messages` — `transcript.rs:620-637` | **Yes, append-only** | **Yes** | Pure function of the persisted rows. A reloaded thread reproduces the same bytes: `Transcript::from_rows` → same parts → same blocks. |
| `messages` tool results | `transcript::tool_result` — `transcript.rs:683-702` → `PythonTool::stored_output` → `model_output` (`tools/python.rs:177-237`) | **Yes** | **Yes** | `clamp_for_model` and the figure budget (`MAX_MODEL_FIGURE_BYTES`) are deterministic functions of the *stored* value, not of wall time or of live kernel state. This is the property that makes reload-after-restart cache-safe. |
| `max_tokens` | const `32_000` — `turn.rs:48` | Yes | Yes | Not part of the cache key anyway. |
| `reasoning` / `thinking` | `id.spec().default_reasoning`, `model/mod.rs:279` | Yes | Yes | Constant per model. Toggling thinking would invalidate tools+system but not messages — we never toggle it mid-thread. |

### Cache-busters found: **none in the prefix**

I went looking for the usual suspects and did not find them:

- **No timestamps.** Nothing calls `SystemTime::now()` / `chrono` on the prompt path.
- **No transport position, no venue listing, no fixture inventory in the prompt.** The venue reaches the model only through `luma.venue` *inside the Python kernel*, i.e. through tool *results*, which are suffix.
- **No binding manifest / revision in the prefix.** `prompts/python-tool.md:1` says the namespace "is refreshed before every call" — that refresh happens in the kernel, not in the tool description. The manifest never enters the request body.
- **No unsorted maps.** See the `preserve_order` note above.
- **No per-thread or per-user id** interpolated anywhere in `system` or `tools`.
- **No conditional system sections.** `system_prompt()` is a two-arm match on a compile-time enum.

### Adjacent smells found (not caching bugs, but flag them)

1. **`system` is a `String`, not a block list.** `ModelRequest.system: String` (`model/mod.rs:101`) is lowered at `model/anthropic.rs:95` as `"system": request.system`. A plain string cannot carry `cache_control`. This is the one structural change caching requires: `system` must become `Vec<ContentBlock>` (or at minimum the transport must emit the one-element array form).

2. **Prompt/tool drift.** `prompts/track.md:25` instructs the model to use "the `skill` tool", and `prompts/graph.md` names `graph_view`, `list_types`, `run_graph`, `preview`, `ask_venue`, `set_args`, `set_preview_selection` — **none of which are in the registry**. `tools::registry` (`tools/mod.rs:129-137`) returns only `PythonTool` for both kinds. The comment at `tools/mod.rs:131-133` acknowledges the graph case; the `skill` reference in `track.md` is not acknowledged anywhere. This is byte-stable, so it costs nothing in cache terms — it just tells the model to call tools that will fail. Worth a separate fix.

3. **`Usage` merge is last-write-wins, not additive** (`model/anthropic.rs:320-334`). Correct today because Anthropic sends `input_tokens` + the cache pair only at `message_start` and `output_tokens` only at `message_delta`. It is fragile if a provider ever re-sends a field. The test at `anthropic.rs:349-376` pins the current behaviour.

4. **The Vercel gateway is the default provider** (`Provider::DEFAULT = VercelAiGateway`, `model/mod.rs:203`) — so any caching design that only tests against `api.anthropic.com` tests the path almost nobody takes.

---

## 3. Provider matrix

Luma routes through three providers (`model/mod.rs:185-199`), two of which speak `/v1/messages`:

| Provider | Transport | Anthropic `cache_control` accepted? | Default in Luma |
|---|---|---|---|
| `VercelAiGateway` | `anthropic::AnthropicClient::gateway` → `https://ai-gateway.vercel.sh/v1/messages` | **Yes** — manual `cache_control` markers pass through to Anthropic/Vertex/Bedrock. The gateway also offers a `caching: "auto"` extension that inserts markers for you. | **Yes** (`Provider::DEFAULT`) |
| `Anthropic` | `AnthropicClient::new` → `https://api.anthropic.com/v1/messages` | Yes, natively | Opt-in only |
| `OpenRouter` | `OpenRouterClient` → `/v1/chat/completions` (OpenAI shape) | Yes **for Anthropic models** — OpenRouter forwards per-block `cache_control` in the OpenAI content-part shape. Non-Anthropic models cache implicitly. | Fallback for models Anthropic doesn't serve |

### Per-model

| Luma model key | Anthropic | OpenRouter | Gateway | Caching mode | Min cacheable prefix |
|---|---|---|---|---|---|
| `claude-opus-5` | `claude-opus-5` | `anthropic/claude-opus-5` | `anthropic/claude-opus-5` | **Explicit** — needs `cache_control` | **512 tokens** |
| `kimi-k3-fast` | — | `moonshotai/kimi-k3-fast` | `moonshotai/kimi-k3-fast` | **Implicit** — Moonshot caches automatically on a stable prefix, no markers, read at 0.25× | not published |
| `grok-4.5` | — | `x-ai/grok-4.5` | `xai/grok-4.5` | **Implicit** — xAI caches automatically, read at 0.25× | not published |

Anthropic minimums are **not monotonic across generations** — Opus 5 is 512, Opus 4.8 / Sonnet 5 / Sonnet 4.6 are 1024, Opus 4.7 is 2048, Opus 4.6 / 4.5 / Haiku 4.5 are 4096. If `MODELS` ever gains an Opus 4.6-class entry, a prefix that caches on Opus 5 will silently stop caching there. Our prefix is comfortably over all of them anyway:

- `track.md` 6757 B + `python-tool.md` 2763 B + the `PythonArgs` schema ≈ **~2.4K tokens** of tools+system for `TrackCopilot`.
- `graph.md` 3515 B + `python-tool.md` 2763 B ≈ **~1.6K tokens** for `PatternGraph`.

Both clear 1024. Both clear 512. Neither clears 4096 — worth knowing if the model table ever grows.

### TTL

| TTL | Write cost | Read cost | Break-even |
|---|---|---|---|
| `ephemeral` (5m, default) | 1.25× base input | 0.1× | 2 requests |
| `ephemeral` + `ttl: "1h"` | 2× base input | 0.1× | 3 requests |

**Recommendation: start with the 5-minute default**, which is also Pi's (`CacheRetention::Short`). An agentic turn fires a model step every few seconds — tool call, result, next step — so the cache is re-read and re-written continuously inside a turn and never gets near the 5-minute wall. The gap that matters is *between* turns: a person reading the lighting result, thinking, and typing again. That gap is frequently longer than 5 minutes in this app, which is the argument *for* `1h`.

The honest answer is that we don't know our inter-turn gap distribution, and we can measure it: the transcript already stores per-step `durationMs` and rows are ordered. Ship 5m, instrument the gap, and switch to `1h` if the p50 inter-turn gap exceeds ~5 minutes. Do **not** ship `1h` on a guess — it doubles the write cost on every step of every turn, and steps inside a turn are where almost all our writes happen.

**Note on the gateway:** its `caching: "auto"` mode is documented as applying only the 5-minute lifetime on the Messages API (`cache_ttl` is a Responses-API field). Another reason to place our own markers rather than delegate.

### Breakpoint budget

**Max 4 `cache_control` breakpoints per request**, on any content block (system text, tool definition, `text`, `image`, `tool_use`, `tool_result`). Each breakpoint walks backward at most **20 content blocks** looking for a prior entry — relevant to us, because one agentic assistant row can easily emit more than 20 blocks (a `tool_use` + a `tool_result` with a text block and several figure images, per step).

---

## 4. Proposed design

### Where the markers go

Port Pi's scheme exactly. Three markers, recomputed from scratch on every request, no persisted state, no rotation:

1. **System.** `cache_control` on every `system` text block — for us that is one block. Because render order is `tools → system → messages`, this covers tools+system together and survives *every* message-level change. (Pi's second system marker exists only for its OAuth identity block; we have no analogue, so we spend one.)

2. **Tools tail.** `cache_control` on the **last** tool definition in `tools[]` — `index == tools.len() - 1`, exactly `anthropic-messages.ts:1355-1361`. With one tool this is the only tool; the rule must still be written as "last" so a second tool needs no code change.

3. **Conversation tail.** `cache_control` on the last content block of the last message, **only when that message is a `User` message** and the block is `Text` / `Image` / `ToolResult` — `anthropic-messages.ts:1295-1317`. In our `to_model_messages` output, tool results are already collapsed into a `ModelRole::User` message (`transcript.rs:653-657`), so during a tool loop this lands on the newest `tool_result`, which is exactly Pi's behaviour. If the last message is an assistant turn, place nothing.

Total: **3 breakpoints**, one under Anthropic's ceiling. The 4th stays unspent.

**On spending the 4th:** the tempting use is a trailing-second anchor on the previous turn's tail, as insurance against the 20-block lookback (see §5 — our figure-heavy tool results make this a real hazard, more so than in Pi, which does not stream images back). Pi deliberately does *not* do this: it accepts the uncovered-prefix case and **measures the waste** with `cache-stats.ts` instead. Follow Pi first. Ship the three markers, port the miss detector, and only add a fourth anchor if the detector shows misses above its noise floor. Adding an anchor we haven't proven we need is how a "one canonical way" turns into two.

### The retention knob

Port `CacheRetention` as a Rust enum on `ModelRequest`:

```rust
enum CacheRetention { None, Short, Long }   // default: Short
```

`None` drops every marker. `Short` is bare `{"type":"ephemeral"}`. `Long` is `{"type":"ephemeral","ttl":"1h"}`, gated on a per-model capability the way Pi gates on `compat.supportsLongCacheRetention` — which in our world is a column on `ModelSpec`, beside `anthropic` / `openrouter` / `gateway` / `context_window`. That keeps the one-model-table invariant `model/mod.rs:1-7` is built around.

`None` earns its place the moment we grow a compaction or summarization fork: Pi forces it there (`compaction.ts:586-593`) so a one-off request doesn't pay a cache write. We have no such fork yet, so `None` is currently only reachable from tests — build it anyway rather than retrofitting the enum later.

### What has to move out of the prefix

**Nothing.** That is the audit's headline. No timestamp, no manifest revision, no venue listing, no sorted-vs-unsorted map is in `system` or `tools` today. The work is entirely additive.

### The one structural change

`ModelRequest.system: String` must become a block list so a marker can attach:

- `model/mod.rs:101` — `pub system: String` → `pub system: Vec<ContentBlock>` (or a narrower `SystemBlock`).
- `model/anthropic.rs:95` — `"system": request.system` → the array form, with `cache_control` on the last element.
- `model/openrouter.rs:62` — currently flattens system into `messages[0]`; for Anthropic-via-OpenRouter the marker rides on the content part there instead.
- `turn.rs:167` — construct the block list.

The alternative — leaving `system` a `String` and having the transport wrap it — is *less* code but it hides the cache decision inside the transport, where three transports would each have to make it. Pull the complexity down into `ModelRequest` once. The transport's job stays "lower this shape onto that wire."

**Where the breakpoint decision itself lives** is the design question worth settling before implementation. Two candidates:

- **(a) In `ModelRequest`.** Add `cache_retention: CacheRetention`; each transport decides *where* to place markers per its own wire shape. This is what Pi does — the knob is provider-independent, the placement is per-API-file.
- **(b) Per-block flags.** Make `cache_control` a field on `ContentBlock`, decided by `turn.rs`. Con: `ContentBlock` grows a field only some providers honour, and the turn would have to know that Anthropic collapses tool results into a user message.

Take **(a)**, i.e. Pi's split. The knob crosses the seam; the placement does not. The placement rule is then written twice — once in `anthropic.rs`, once in `openrouter.rs` — which is exactly what Pi accepts, and it is right here for the same reason: the two wires disagree about where the last cacheable block *is* (`openrouter.rs:107-167` splits one `ModelMessage` into several completions messages, so "last block of the last message" means something different on each side). One shared placement function over two different message shapes would be a false abstraction.

For OpenRouter, port the model-id gate verbatim (`openai-completions.ts:1621`): place markers only when the wire id starts with `anthropic/`. Our `ModelSpec.openrouter` already carries that string (`model/mod.rs:277`), so the check is one line and needs no new column.

### Non-Anthropic models

`kimi-k3-fast` and `grok-4.5` cache implicitly on a stable prefix. Our prefix already is stable, so they should already be caching — and `openrouter.rs:205-207` already reads `prompt_tokens_details.cached_tokens` back into `cache_read_input_tokens`. **Before writing any code, check the usage card on a second Kimi turn.** If it shows zero cache read, something in the OpenAI-shaped lowering is unstable and that is a separate bug worth finding.

Do **not** emit `cache_control` for these models. It is at best ignored, at worst a 400.

### MCP-driven turns are unaffected

`src-tauri/src/bin/luma-mcp.rs` exposes the kernel to an out-of-process coding agent. There is no in-app loop there — Luma never assembles a `ModelRequest`, so there is nothing to cache and nothing to break. The one shared surface is `PYTHON_TOOL_DESCRIPTION` (`tools/python.rs:29`), which is `pub` precisely so both hosts hand the model the same text. Keeping it a `const &'static str` from a file is the caching invariant *and* the single-source-of-truth invariant; they point the same way.

Subagents likewise: there is no subagent turn loop in the Rust agent yet (`TurnEvent::Subagent` at `agent/mod.rs:484` is live UI state only). When one lands, the rule from §1 applies — a fork must reuse the parent's `system`, `tools`, and `model` **verbatim** and append only at the tail, or it misses the parent's cache entirely.

### Acceptance test

**The manual acceptance test, run through the real app:**

1. Open a fresh track thread on `claude-opus-5` through the default provider (Vercel gateway).
2. Turn one: any prompt. Open the usage card. Record `Prompt` (`RequestUsage::prompt_tokens`, `transcript.rs:428-432`) and `Cache write`.
3. Turn two, same thread. **Assert `Cache read` ≈ turn one's `Prompt`**, and `Input` is only the new user text.
4. Repeat against `Provider::Anthropic` and against `kimi-k3-fast` over OpenRouter.

The usage card already renders exactly these rows — `gpui/crates/chat/src/usage.rs:186-190` shows `Input` / `Cache read` / `Cache write` / `Output` / `Prompt`. No new instrument is needed.

**The automated test**, modelled on `a_live_openrouter_turn_reports_both_halves_of_its_usage` (`model/mod.rs:654-688`) and `a_live_gateway_tool_call_round_trips` (`model/mod.rs:703+`):

```
#[tokio::test]
#[ignore = "live: needs LUMA_AI_GATEWAY_API_KEY and a network"]
async fn a_second_step_reads_the_prefix_its_first_step_wrote()
```

Two steps against the **real** shipped registry specs and the **real** `AgentKind::TrackCopilot` system prompt — not a hand-written stand-in, for the same reason `a_live_gateway_tool_call_round_trips` uses `tools::registry(...).specs()`: the prefix a real turn sends is the thing under test. Step one asserts `cache_creation_input_tokens > 0`. Step two appends one user block and asserts `cache_read_input_tokens >= step_one.prompt_tokens() * 0.9` and `input_tokens < 500`.

Add a **non-live** companion that asserts the request body's shape: exactly three markers, on the last tool, the system block, and the last content block of the last message — and **zero** when that last message is an assistant turn, and **zero** for a non-`anthropic/` OpenRouter wire id. This is the same way `the_request_body_carries_tools_and_an_effort_level` (`anthropic.rs:428-449`) pins the body today, and it mirrors Pi's own executable spec (`packages/ai/test/openai-completions-cache-control-format.test.ts` asserts exactly three markers and that "the conversation cache marker moves to a tool result"). This is the test that catches a regression without spending tokens, and it is the one that would have caught "someone inlined a catalog into the tool description".

**And port the miss detector.** `cache-stats.ts` is the durable answer to "is caching still working", as opposed to a one-time acceptance check. We already have everything it needs: `Transcript::last_request` reads `RequestUsage` per step, and each `data-pi-message` part carries `usage` + `durationMs`. Walking the transcript gives `missedTokens = min(prev_prompt, prompt) - cache_read` per step for free, with Pi's `NOISE_FLOOR_TOKENS = 1024` and its sticky "this provider never reports caching" flag. It belongs beside `RequestUsage` in `transcript.rs`, surfaced in the usage card that already renders the raw counters — not as a second panel.

---

## 5. Risks

**Binding manifest changes mid-thread.** Low risk *today*: the manifest is refreshed inside the Python kernel and never enters the request body (`prompts/python-tool.md:1`). The risk is a future change that inlines a catalog into the tool description "so the model doesn't have to call `luma.catalog()`". That would make the description a function of the thread's scope and destroy the cache on every venue/track switch — and the description sits at position 0, so it destroys *everything*. The comment at `tools/python.rs:23-25` is the guard; it should be treated as a hard rule and the acceptance test above is what enforces it.

**Image-heavy tool results.** This is the real one. A matplotlib-heavy cell can push 6 MB of base64 (`MAX_MODEL_FIGURE_BYTES`) into a single `tool_result` as several `Image` blocks. Two consequences:

- **20-block lookback.** One assistant row with 4 tool calls × (1 `tool_use` + 1 text + 2 figures) already exceeds 20 blocks. The moving breakpoint on the newest block then fails to find the previous entry and silently misses. This is the argument for spending the 4th breakpoint on a trailing-second marker.
- **Cache-write cost.** Images are tokens, and at 1.25× write they are expensive tokens. This does not argue against caching — re-sending them uncached every step is worse — but it does argue against `ttl: "1h"` (2×) until we've measured.

**Steering leaves a turn's prefix uncovered.** `turn.rs:143-146` appends a steering message between assistant rows, so the next request's tail is a fresh user message and the marker moves there. That is fine. The uncovered case is the mirror of Pi's accepted tradeoff (§1, §7): when the last message is an **assistant** turn, no message marker is placed at all, and that step reads only up to system+tools. Pi accepts this and measures it. We should too — it is precisely what the ported miss detector will show, and if it turns out to be frequent in our loop (it should not be, since our `assistant_row` always ends a step with either tool results or a final answer) that is the evidence for spending the 4th breakpoint.

**Thread reload after restart.** Should be **safe**, and this is a property worth stating explicitly because it is not obvious: `to_model_messages` is pure over the persisted rows, `stored_output` is pure over the stored JSON, `system_prompt()` and the tool description are `include_str!` constants compiled into the binary. A thread reopened tomorrow rebuilds byte-identical bytes. **Except across a build:** editing `track.md` or `python-tool.md` changes the prefix for every existing thread. That is correct behaviour (the model should see the new prompt) but it means every thread pays one cold write after a prompt edit. Not a bug; worth knowing before someone reports "caching broke after the update".

**Model switch mid-thread.** Caches are model-scoped and there is no escape hatch. `agent_model` is a global setting (`model/mod.rs:317-326`), so a user changing models cold-starts every open thread. Acceptable; not worth engineering around.

**Provider switch mid-thread.** Same shape, and slightly worse: switching from the gateway to direct Anthropic changes the endpoint and the cache is not shared. Also acceptable.

**The gateway's `caching: "auto"`.** Tempting — one field, no code. Reject it: it only offers the 5-minute lifetime on the Messages API, it places markers by *its* heuristic rather than ours, and it works on exactly one of our three providers. Placing our own markers is the same amount of work and works everywhere.
