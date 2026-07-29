import { safeValidateUIMessages, type UIMessage } from "ai";
import type {
	AgentThread,
	AgentThreadDetail,
	AgentThreadMessage,
	CreateAgentThreadInput,
	NewAgentThreadMessage,
} from "@/bindings/schema";
import { invoke } from "@/shared/lib/tauri";

/**
 * Typed client for the durable agent-thread commands.
 *
 * A thread is the identity an agent conversation (and, later, its Python
 * workspace) hangs off. The subject association — track or pattern — is
 * metadata: several threads may exist for one subject, and `resolveThread`
 * simply picks the most recently touched one, or creates the first.
 */

export type AgentKind = "track_copilot" | "pattern_graph";
export type SubjectKind = "track" | "pattern";

export type ThreadInit = {
	venueId?: string | null;
	scoreId?: string | null;
	title?: string | null;
};

// --------------------------------------------------------------------------
// Command wrappers
// --------------------------------------------------------------------------

export function createThread(
	input: CreateAgentThreadInput,
): Promise<AgentThread> {
	return invoke<AgentThread>("agent_thread_create", { input });
}

export function getThread(threadId: string): Promise<AgentThreadDetail> {
	return invoke<AgentThreadDetail>("agent_thread_get", { threadId });
}

export function listThreads(filter: {
	agentKind?: AgentKind;
	subjectKind?: SubjectKind;
	subjectId?: string;
}): Promise<AgentThread[]> {
	return invoke<AgentThread[]>("agent_thread_list", {
		agentKind: filter.agentKind ?? null,
		subjectKind: filter.subjectKind ?? null,
		subjectId: filter.subjectId ?? null,
	});
}

export function appendMessages(
	threadId: string,
	messages: NewAgentThreadMessage[],
): Promise<AgentThreadMessage[]> {
	return invoke<AgentThreadMessage[]>("agent_thread_append_messages", {
		threadId,
		messages,
	});
}

export function truncateFrom(threadId: string, seq: number): Promise<number> {
	return invoke<number>("agent_thread_truncate_from", { threadId, seq });
}

/** Clear a thread's messages, keeping its identity (and, later, atomically
 * resetting the thread's Python workspace) — one command, one transaction. */
export function resetThread(threadId: string): Promise<number> {
	return invoke<number>("agent_thread_reset", { threadId });
}

export function deleteThread(threadId: string): Promise<void> {
	return invoke<void>("agent_thread_delete", { threadId });
}

export function renameThread(
	threadId: string,
	title: string | null,
): Promise<AgentThread> {
	return invoke<AgentThread>("agent_thread_rename", { threadId, title });
}

// --------------------------------------------------------------------------
// Resolution
// --------------------------------------------------------------------------

/** Most recently touched first. `updatedAt` ties break on `createdAt`. */
export function newestThread(threads: AgentThread[]): AgentThread | null {
	let best: AgentThread | null = null;
	for (const t of threads) {
		if (
			best === null ||
			t.updatedAt > best.updatedAt ||
			(t.updatedAt === best.updatedAt && t.createdAt > best.createdAt)
		) {
			best = t;
		}
	}
	return best;
}

/**
 * The newest existing thread for `(agentKind, subjectKind, subjectId)`, or a
 * freshly created one. The backend already orders newest-first, but we pick
 * explicitly so the choice doesn't depend on SQL ordering.
 */
export async function resolveThread(
	agentKind: AgentKind,
	subjectKind: SubjectKind,
	subjectId: string,
	init: ThreadInit = {},
): Promise<AgentThread> {
	const existing = await listThreads({ agentKind, subjectKind, subjectId });
	const newest = newestThread(existing);
	if (newest) return newest;
	return createThread({
		agentKind,
		subjectKind,
		subjectId,
		venueId: init.venueId ?? null,
		scoreId: init.scoreId ?? null,
		title: init.title ?? null,
	});
}

// --------------------------------------------------------------------------
// Loading
// --------------------------------------------------------------------------

/** What the frontend keeps about the rows already in the database, so the next
 * persist can diff against them without re-reading the thread. */
export type PersistedMessage = {
	id: string;
	seq: number;
	role: string;
	/** Canonical JSON of the persisted `parts`, for change detection. */
	partsJson: string;
};

export type LoadedThread = {
	messages: UIMessage[];
	baseline: PersistedMessage[];
};

/**
 * Pair persisted rows with the client-side messages they correspond to.
 *
 * `partsJson` is deliberately computed from the *client* message, not from the
 * row echoed back by the backend: `parts` round-trips through `serde_json`,
 * which may reorder object keys, and a baseline that doesn't stringify
 * identically to the next in-memory diff would truncate and re-append the whole
 * thread on every save. Only `seq` is authoritative from the backend.
 */
function toBaseline(
	rows: Pick<AgentThreadMessage, "seq">[],
	messages: UIMessage[],
): PersistedMessage[] {
	const n = Math.min(rows.length, messages.length);
	return messages.slice(0, n).map((m, i) => ({
		id: m.id,
		seq: rows[i].seq,
		role: m.role,
		partsJson: JSON.stringify(m.parts),
	}));
}

/**
 * Load a thread's history as validated `UIMessage[]`.
 *
 * Persisted parts are opaque to the backend, so a message written by an older
 * build (or a half-written tool part) can fail validation. That must never take
 * the chat down: we drop to the longest valid prefix, and if even that fails,
 * start empty. The baseline always describes exactly the rows we kept, so the
 * next persist truncates the rest rather than silently diverging.
 */
export async function loadThreadMessages(
	threadId: string,
): Promise<LoadedThread> {
	const detail = await getThread(threadId);
	const rows = detail.messages;
	const raw = rows.map((m) => ({ id: m.id, role: m.role, parts: m.parts }));
	// A fresh thread has nothing to validate, and `safeValidateUIMessages`
	// rejects an empty array outright — don't treat "new" as "corrupt".
	if (raw.length === 0) return { messages: [], baseline: [] };

	const validated = await safeValidateUIMessages<UIMessage>({ messages: raw });
	if (validated.success) {
		return {
			messages: validated.data,
			baseline: toBaseline(rows, validated.data),
		};
	}

	console.warn(
		`[agent-threads] thread ${threadId} failed message validation; ` +
			`falling back to the longest valid prefix.`,
		validated.error,
	);

	for (let n = raw.length - 1; n > 0; n--) {
		const prefix = await safeValidateUIMessages<UIMessage>({
			messages: raw.slice(0, n),
		});
		if (prefix.success) {
			return {
				messages: prefix.data,
				baseline: toBaseline(rows.slice(0, n), prefix.data),
			};
		}
	}

	console.warn(`[agent-threads] thread ${threadId} starting empty.`);
	return { messages: [], baseline: [] };
}

// --------------------------------------------------------------------------
// Persistence
// --------------------------------------------------------------------------

export type ThreadSyncPlan = {
	/** Delete every persisted message with `seq >= truncateFromSeq` first. */
	truncateFromSeq: number | null;
	append: UIMessage[];
};

/**
 * Diff the in-memory messages against what's already persisted.
 *
 * The common case is append-only: a finished turn adds a user message and an
 * assistant message to the end. When history was edited (a message rewritten,
 * removed, or reordered) the shared prefix ends early, and everything from
 * there on is truncated and re-appended — the ids and seqs stay contiguous
 * instead of accumulating orphans.
 */
export function planThreadSync(
	baseline: PersistedMessage[],
	current: UIMessage[],
): ThreadSyncPlan {
	let shared = 0;
	while (shared < baseline.length && shared < current.length) {
		const b = baseline[shared];
		const c = current[shared];
		if (b.id !== c.id || b.role !== c.role) break;
		if (b.partsJson !== JSON.stringify(c.parts)) break;
		shared += 1;
	}
	return {
		truncateFromSeq: shared < baseline.length ? baseline[shared].seq : null,
		append: current.slice(shared),
	};
}

/**
 * Bring the thread's stored history in line with `current`, and return the
 * baseline to diff against next time. A no-op plan costs zero commands.
 */
export async function syncThreadMessages(
	threadId: string,
	baseline: PersistedMessage[],
	current: UIMessage[],
): Promise<PersistedMessage[]> {
	const plan = planThreadSync(baseline, current);
	if (plan.truncateFromSeq === null && plan.append.length === 0) {
		return baseline;
	}

	const kept =
		plan.truncateFromSeq === null
			? baseline
			: baseline.filter((m) => m.seq < (plan.truncateFromSeq as number));
	if (plan.truncateFromSeq !== null) {
		await truncateFrom(threadId, plan.truncateFromSeq);
	}
	if (plan.append.length === 0) return kept;

	const written = await appendMessages(
		threadId,
		plan.append.map((m) => ({
			id: m.id,
			role: m.role,
			parts: m.parts as unknown[],
		})),
	);
	return [...kept, ...toBaseline(written, plan.append)];
}
