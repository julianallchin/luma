import type {
	AgentThread,
	AgentThreadDetail,
	AgentThreadMessage,
	AppendAgentThreadMessagesInput,
	CreateAgentThreadInput,
} from "@/bindings/schema";
import { invoke } from "@/shared/lib/tauri";
import { type AgentChatMessage, isAgentChatMessage } from "./messages";

/**
 * Typed client for the durable agent-thread commands.
 *
 * A thread is the identity an agent conversation (and, later, its Python
 * workspace) hangs off. The subject association — track or pattern — is
 * metadata: several threads may exist for one subject and editing scope. A
 * track conversation is pinned to its account principal, venue, and persisted
 * track revision owner (`scoreId` internally); changing any of them resolves a
 * different thread. The principal supplied here partitions frontend memory;
 * backend ownership always comes from trusted authenticated state.
 */

export type AgentKind = "track_copilot" | "pattern_graph";
export type SubjectKind = "track" | "pattern";

export type ThreadInit = {
	/** Frontend cache partition only. The backend derives the authoritative
	 * owner from authenticated host state and never trusts this value. */
	principalId: string | null;
	implementationId?: string | null;
	venueId?: string | null;
	scoreId?: string | null;
	title?: string | null;
};

/**
 * The exact identity boundary for resolving and reopening a conversation.
 * Every nullable field is normalized explicitly: omission must never become a
 * wildcard that can reuse a differently pinned thread. Title is deliberately
 * absent because it is display metadata, not identity.
 */
export type ExactThreadScope = {
	principalId: string | null;
	agentKind: AgentKind;
	subjectKind: SubjectKind;
	subjectId: string;
	implementationId: string | null;
	venueId: string | null;
	scoreId: string | null;
};

export function normalizeThreadScope(
	agentKind: AgentKind,
	subjectKind: SubjectKind,
	subjectId: string,
	init: ThreadInit = { principalId: null },
): ExactThreadScope {
	const implementationId = init.implementationId ?? null;
	if (agentKind === "pattern_graph" && implementationId === null) {
		throw new Error("Pattern graph threads require a concrete implementation.");
	}
	if (agentKind === "track_copilot" && implementationId !== null) {
		throw new Error("Track threads cannot carry a graph implementation.");
	}
	return {
		principalId: init.principalId,
		agentKind,
		subjectKind,
		subjectId,
		implementationId,
		venueId: init.venueId ?? null,
		scoreId: init.scoreId ?? null,
	};
}

/**
 * Client-side fail-closed validation for a row returned by the trusted
 * backend. Backend ownership remains authoritative; this prevents an async
 * response or in-memory cache entry from being activated under another scope.
 */
export function threadMatchesScope(
	thread: AgentThread,
	scope: ExactThreadScope,
): boolean {
	return (
		thread.ownerUserId === scope.principalId &&
		thread.agentKind === scope.agentKind &&
		thread.subjectKind === scope.subjectKind &&
		thread.subjectId === scope.subjectId &&
		thread.implementationId === scope.implementationId &&
		thread.venueId === scope.venueId &&
		thread.scoreId === scope.scoreId
	);
}

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

function appendMessages(
	threadId: string,
	input: AppendAgentThreadMessagesInput,
): Promise<AgentThreadMessage[]> {
	return invoke<AgentThreadMessage[]>("agent_thread_append_messages", {
		threadId,
		input,
	});
}

export async function deleteThread(threadId: string): Promise<void> {
	await invoke<void>("agent_thread_delete", { threadId });
	pendingTranscriptAppends.delete(threadId);
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

/** List only threads belonging to one exact frontend scope. */
export async function listScopedThreads(
	scope: ExactThreadScope,
): Promise<AgentThread[]> {
	const threads = await listThreads({
		agentKind: scope.agentKind,
		subjectKind: scope.subjectKind,
		subjectId: scope.subjectId,
	});
	return threads.filter((thread) => threadMatchesScope(thread, scope));
}

/**
 * Create a thread pinned to `scope`. The principal is intentionally not sent:
 * the backend captures it from authenticated host state. Validate the returned
 * row before allowing it into the frontend cache.
 */
export async function createScopedThread(
	scope: ExactThreadScope,
	title: string | null = null,
	requestId = crypto.randomUUID(),
): Promise<AgentThread> {
	const input: CreateAgentThreadInput = {
		requestId,
		agentKind: scope.agentKind,
		subjectKind: scope.subjectKind,
		subjectId: scope.subjectId,
		implementationId: scope.implementationId,
		venueId: scope.venueId,
		scoreId: scope.scoreId,
		title,
	};
	let thread: AgentThread;
	try {
		thread = await createThread(input);
	} catch {
		// The backend binds this exact request ID durably. Retrying the same
		// payload can recover an applied response without creating another task.
		thread = await createThread(input);
	}
	if (!threadMatchesScope(thread, scope)) {
		throw new Error(
			`Created agent thread '${thread.id}' does not match the requested scope.`,
		);
	}
	return thread;
}

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
	init: ThreadInit = { principalId: null },
): Promise<AgentThread> {
	const scope = normalizeThreadScope(agentKind, subjectKind, subjectId, init);
	const newest = newestThread(await listScopedThreads(scope));
	if (newest) return newest;
	return createScopedThread(scope, init.title ?? null);
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
	messages: AgentChatMessage[];
	baseline: PersistedMessage[];
};

/**
 * Pair persisted rows with the client-side messages they correspond to.
 *
 * `partsJson` is deliberately computed from the *client* message, not from the
 * row echoed back by the backend: `parts` round-trips through `serde_json`,
 * which may reorder object keys, and a baseline that doesn't stringify
 * identically to the next in-memory comparison would falsely report durable
 * divergence. Row identity, role, and sequence are still verified against the
 * backend response.
 */
function toBaseline(
	rows: Pick<AgentThreadMessage, "id" | "seq" | "role">[],
	messages: AgentChatMessage[],
): PersistedMessage[] {
	if (rows.length !== messages.length) {
		throw new Error(
			`Transcript persistence returned ${rows.length} rows for ${messages.length} messages.`,
		);
	}
	let previousSeq = Number.NEGATIVE_INFINITY;
	return messages.map((message, index) => {
		const row = rows[index];
		if (
			row.id !== message.id ||
			row.role !== message.role ||
			row.seq <= previousSeq
		) {
			throw new Error(
				`Transcript persistence returned a mismatched row at message ${index + 1}.`,
			);
		}
		previousSeq = row.seq;
		return {
			id: message.id,
			seq: row.seq,
			role: message.role,
			partsJson: JSON.stringify(message.parts),
		};
	});
}

function baselineFromAppend(
	threadId: string,
	baseline: PersistedMessage[],
	rows: AgentThreadMessage[],
	messages: AgentChatMessage[],
): PersistedMessage[] {
	if (rows.some((row) => row.threadId !== threadId)) {
		throw new Error("Transcript append returned a row for another thread.");
	}
	const appended = toBaseline(rows, messages);
	let expectedSeq = baseline.at(-1)?.seq ?? -1;
	for (const message of appended) {
		expectedSeq += 1;
		if (message.seq !== expectedSeq) {
			throw new Error("Transcript append returned a non-contiguous sequence.");
		}
	}
	return appended;
}

/**
 * Load a thread's history as validated frontend transcript messages.
 *
 * Persisted parts are opaque to the backend, so a message written by an older
 * build can fail validation in the current client. Fail closed in that case:
 * presenting a partial history would make it look safe to append after rows
 * that this build merely failed to understand.
 */
export async function loadThreadMessages(
	threadId: string,
): Promise<LoadedThread> {
	const detail = await getThread(threadId);
	const rows = detail.messages;
	const raw = rows.map((m) => ({
		id: m.id,
		role: m.role,
		parts:
			m.parts.length === 0 ? [{ type: "text" as const, text: "" }] : m.parts,
	}));
	// A fresh thread has nothing to validate — don't treat "new" as corrupt.
	if (raw.length === 0) {
		pendingTranscriptAppends.delete(threadId);
		return { messages: [], baseline: [] };
	}

	if (raw.every(isAgentChatMessage)) {
		const baseline = toBaseline(rows, raw);
		pendingTranscriptAppends.delete(threadId);
		return {
			messages: raw,
			baseline,
		};
	}

	throw new Error(
		`Conversation ${threadId} contains messages this version cannot validate; its durable transcript was left unchanged.`,
	);
}

// --------------------------------------------------------------------------
// Persistence
// --------------------------------------------------------------------------

export type ThreadAppendPlan = {
	append: AgentChatMessage[];
};

type PendingTranscriptAppend = {
	requestKey: string;
	operationId: string;
};

/**
 * One in-flight idempotency key per thread. If IPC loses a response, retrying
 * the same append reuses the key and receives the exact committed result. Once
 * the response arrives the key is retired, so a later append with identical
 * content is still a new operation.
 * After a process restart the thread is reloaded from SQLite: a committed
 * append is already reflected in its baseline and an uncommitted one is safe
 * to issue under a fresh key.
 */
const pendingTranscriptAppends = new Map<string, PendingTranscriptAppend>();

function operationForAppend(
	threadId: string,
	baseline: PersistedMessage[],
	request: Omit<AppendAgentThreadMessagesInput, "operationId">,
): PendingTranscriptAppend {
	const requestKey = JSON.stringify({ baseline, request });
	const pending = pendingTranscriptAppends.get(threadId);
	if (pending) {
		if (pending.requestKey === requestKey) return pending;
		throw new Error(
			"A previous transcript append is unresolved; reload the conversation before appending different content.",
		);
	}
	const next = {
		requestKey,
		operationId: globalThis.crypto.randomUUID(),
	};
	pendingTranscriptAppends.set(threadId, next);
	return next;
}

/**
 * Diff the in-memory messages against what's already persisted.
 *
 * Persisted messages must remain an exact prefix. Redo adds a new turn, and
 * rewind is an explicit authored-state restore; neither operation edits the
 * durable conversation log.
 */
export function planThreadAppend(
	baseline: PersistedMessage[],
	current: AgentChatMessage[],
): ThreadAppendPlan {
	if (current.length < baseline.length) {
		throw new Error(
			"The durable transcript is not an exact prefix of the current conversation.",
		);
	}
	for (let index = 0; index < baseline.length; index += 1) {
		const persisted = baseline[index];
		const message = current[index];
		if (
			persisted.id !== message.id ||
			persisted.role !== message.role ||
			persisted.partsJson !== JSON.stringify(message.parts)
		) {
			throw new Error(
				`The durable transcript diverged from the current conversation at message ${index + 1}.`,
			);
		}
	}
	return { append: current.slice(baseline.length) };
}

/**
 * Bring the thread's stored history in line with `current`, and return the
 * baseline to diff against next time. A no-op plan costs zero commands.
 */
export async function appendThreadMessages(
	threadId: string,
	baseline: PersistedMessage[],
	current: AgentChatMessage[],
): Promise<PersistedMessage[]> {
	const plan = planThreadAppend(baseline, current);
	if (plan.append.length === 0) return baseline;

	const request = {
		expectedHeadMessageId: baseline.at(-1)?.id ?? null,
		messages: plan.append.map((m) => ({
			id: m.id,
			role: m.role,
			parts: m.parts as unknown[],
		})),
	};
	const pending = operationForAppend(threadId, baseline, request);
	const written = await appendMessages(threadId, {
		operationId: pending.operationId,
		...request,
	});
	const appendedBaseline = baselineFromAppend(
		threadId,
		baseline,
		written,
		plan.append,
	);
	if (pendingTranscriptAppends.get(threadId) === pending) {
		pendingTranscriptAppends.delete(threadId);
	}
	return [...baseline, ...appendedBaseline];
}
