# Prompt caching

Status: built. Code: `backend/src/agent/model/mod.rs` (`CacheRetention`),
`model/anthropic.rs`, `model/openrouter.rs`, `agent/transcript.rs`.

The in-app agent marks the stable prefix of each request with `cache_control`.
The scheme copies Pi (`badlogic/pi-mono`, `packages/ai`). It applies to turns
that Luma sends to a model API. The Codex and Claude CLI engines manage their
own caching.

## The knob

`ModelRequest.cache_retention` is a `CacheRetention`:

| value | markers |
|---|---|
| `None` | no markers |
| `Short` (default) | `{"type": "ephemeral"}`, 5 minutes |
| `Long` | `{"type": "ephemeral", "ttl": "1h"}`, only on a model whose spec allows it; otherwise `Short` |

A turn reads it from `LUMA_CACHE_RETENTION` (`none`, `short`, `long`). An
unknown value gives `Short`.

## Where the markers go

The markers are computed again for every request. There is no stored state.

1. The system block.
2. The last tool definition.
3. The last content block of the last message, only when that message is a
   user message and the block is text, an image or a tool result. During a
   tool loop this is the newest tool result. After an assistant message there
   is no tail marker.

That is three markers. The provider limit is four.

Each transport places the markers for its own wire shape:

- `anthropic.rs` serves the Vercel AI Gateway, which is the default provider.
- `openrouter.rs` adds markers only when the wire id starts with
  `anthropic/`. Other OpenRouter models cache implicitly on a stable prefix,
  and they can reject the field.

## Keeping the prefix stable

- The system prompt and tool descriptions are `include_str!` files. They
  contain no date and no catalog.
- `turn.rs` appends the thread's scope to the system prompt as JSON (track,
  venue, score, pattern and implementation ids). A thread's scope does not
  change, so the prefix stays stable for the life of the thread.
- The skill listing is sorted by name.
- Tool order is fixed by `registry_for_context`.
- `serde_json` is built without `preserve_order`, so maps serialize in key
  order.
- `transcript::to_model_messages` is a pure function of stored rows. A thread
  that is opened again produces the same bytes.

An edit to a prompt file changes the prefix for every thread. The next request
on each thread writes the cache again.

## Measuring misses

`Transcript::missed_cache_tokens` adds up, per step,
`min(previous prompt, prompt) - cache read`. Misses at or below
`CACHE_MISS_NOISE_FLOOR` (1024 tokens) are not counted. A provider that never
reports cache reads shows as a full miss.

Tests: the body-shape tests in `anthropic.rs` and `openrouter.rs` count the
markers. Live tests in `agent/tests.rs` need `LUMA_AI_GATEWAY_API_KEY`.

## Known limits

- Anthropic's lookback from a marker is 20 content blocks. A step with many
  figure blocks can miss the previous entry. The fourth marker is unused.
  Add it only if measured misses show a need.
- A cache is scoped to a model and to a provider. Changing either starts cold.
