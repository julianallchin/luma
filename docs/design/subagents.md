# Subagents

Status: built. Code: `backend/src/agent/subagent.rs`,
`backend/src/agent/tools/subagent.rs`, `backend/src/services/drafts.rs`,
`gpui/crates/chat/src/chip.rs`, `gpui/crates/chat/src/subagents.rs`,
`gpui/crates/app/src/subagents.rs`.

A subagent is an ordinary agent turn that runs on a child thread. It uses the
same service, tools, model and reducer as its parent. The child thread has a
parent, and it edits a draft instead of the live score.

## The tool

`subagent` is in the registry only for a thread that edits a score
(`tools::registry_for_context`). Its description is
`backend/src/agent/prompts/subagent-tool.md`.

| action | arguments | effect |
|---|---|---|
| `delegate` (default) | `description`, `task` | Starts a child turn and waits for it. |
| `inspect` | `childThreadId`, optional `path`, `offset` | Reads a finished child's work. |

`description` is a short label. The chip and the dialog show it. `task` is the
whole brief, because the child cannot see the parent conversation.

## Delegation

1. Check the limits. `MAX_DEPTH = 2`: a child may delegate, a grandchild may
   not. The check walks the thread's own parent chain. `MAX_CONCURRENT = 4`
   children may run at one time per thread. A refused start is a tool error.
2. Create the child `agent_threads` row with `parent_thread_id` and
   `parent_call_id`.
3. If the parent has a score, create a `drafts` row for the child. The child's
   `track.score_apply` writes the draft's `state_json`.
4. Run the child turn. Its messages are ordinary rows in the child thread.
5. On success, `drafts::merge` compares `state_json` with `base_json` per clip
   and per definition. It writes each changed or added row onto the live score,
   removes each row the child deleted, and then deletes the draft. The parent model receives `<subagent status="merged"/>`
   and the child's final text, clamped to 16,000 characters.
6. On failure, the draft is deleted. The tool returns an error that names the
   child thread.

Cancellation needs no extra code. The child stream runs inside the parent's
tool call. When the parent turn is dropped, the child turn is dropped too. A
cancelled child can leave its draft. The draft goes when its thread goes.

## Live state

`SubagentSnapshot` reaches the host as `TurnEvent::Subagent`. It holds the
child thread id, the parent call id, the description, the phase (`Running`,
`Merging`, `Completed`, `Failed`) and one line of activity. It is never stored.
Everything durable is already a row in the child thread.

## UI

- The transcript shows one chip per delegation (`chip.rs`). Its trailing text
  reads the stored tool output: started, finished or failed.
- A pill above the composer counts running children (`chat/src/subagents.rs`).
  It is hidden when the count is zero.
- The pill opens a dialog (`app/src/subagents.rs`). The dialog lists the
  thread's children and opens one child's transcript read-only.

## Known limits

- A merge does not detect conflicts. When the parent and a child change the
  same clip or definition, the child's version wins.
- A reload during a run shows the chips but no pill until the next snapshot
  arrives.
