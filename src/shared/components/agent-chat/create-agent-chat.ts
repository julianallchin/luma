import type { Agent, AgentEvent } from "@earendil-works/pi-agent-core";
import { useCallback, useEffect, useRef, useState } from "react";
import type {
	AgentThread,
	AuthoredProjectedDocument,
	Graph,
} from "@/bindings/schema";
import { type ToolSet, toPiTools } from "@/shared/lib/agent/agent-tool";
import {
	type AuthoredRestoreMode,
	type AuthoredRestoreResult,
	type AuthoredTurnCommit,
	finalizeAuthoredTurn,
	type PreparedAuthoredTurn,
	prepareAuthoredTurn,
	recoverAuthoredTurns,
	restoreAuthoredState,
} from "@/shared/lib/agent/authored-state";
import {
	type AppliedAuthoredSubagent,
	type AuthoredSubagentFinalization,
	AuthoredSubagentSupervisor,
	type PreparedAuthoredSubagent,
} from "@/shared/lib/agent/authored-subagent-supervisor";
import {
	type AgentChatMessage,
	applyAgentEvent,
	userAgentMessage,
	userChatMessage,
} from "@/shared/lib/agent/messages";
import {
	createPiAgent,
	type PiAgentModel,
} from "@/shared/lib/agent/pi-agent-loop";
import {
	type AgentDefinition,
	AgentLoader,
	createPiSubagentRunner,
	createSubagentTools,
	SubagentManager,
	type SubagentSnapshot,
} from "@/shared/lib/agent/subagents";
import {
	type AgentKind,
	appendThreadMessages,
	createScopedThread,
	getThread,
	listScopedThreads,
	loadThreadMessages,
	normalizeThreadScope,
	type PersistedMessage,
	resolveThread,
	type SubjectKind,
	type ThreadInit,
	threadMatchesScope,
} from "@/shared/lib/agent/threads";
import type { ToolVocab } from "./parts";

/**
 * The base agent-chat encapsulation, built on Pi's Agent and event model over
 * durable backend threads.
 *
 * Each concrete agent (graph editor, track editor, …) supplies an
 * `AgentChatSpec` — model + tools + system + display vocab — and gets back a
 * fully-wired chat whose history lives in SQLite.
 *
 * Two keys, deliberately distinct:
 *
 * - **subject scope** — the account principal, thing being worked on, and its
 *   persisted editing scope (principal + patternId/trackId + venueId +
 *   scoreId). Bridges are keyed by it because neither a live editor handle nor
 *   a hydrated transcript may leak into another scoped thread.
 * - **thread id** — the durable conversation identity. Chats are keyed by it,
 *   because that is what the messages (and, later, the Python workspace) hang
 *   off. Several threads may exist for one subject.
 *
 * `Bridge` is the agent's live handle on whatever it reads/mutates. Tools and
 * the transport resolve it lazily (`getBridge()`), so a long-lived session
 * always acts on the current editor.
 */

/** What a spec gets to build its tools with, once per turn. */
export type BuildToolsArgs<Bridge> = {
	getBridge: () => Bridge | null;
	/** The turn's abort signal — pass it to any long-running tool work so
	 * stopping the model also stops the tool. */
	abortSignal: AbortSignal | undefined;
	/** The durable thread this turn belongs to (owns the Python workspace). */
	threadId: string;
	/** The durable user message that began this turn. Mutation tools use this
	 * identity instead of ephemeral provider tool-call ids. */
	turnMessageId: string;
};

/** The normal domain-tool surface rebound to one detached child revision. */
export type BuildSubagentToolsArgs<Bridge> = BuildToolsArgs<Bridge> & {
	subagentId: string;
	workspaceId: string;
	initialDocument: AuthoredProjectedDocument;
	bindWorkspaceDocument: PreparedAuthoredSubagent["bindDocumentSink"];
};

function assertSameDomainToolSurface(
	parentTools: ToolSet,
	childTools: ToolSet,
): void {
	const parentNames = Object.keys(parentTools).sort();
	const childNames = Object.keys(childTools).sort();
	if (
		parentNames.length !== childNames.length ||
		parentNames.some((name, index) => name !== childNames[index])
	) {
		throw new Error(
			`Subagent domain tools must match the parent surface (parent: ${parentNames.join(", ") || "none"}; child: ${childNames.join(", ") || "none"}).`,
		);
	}
}

export type TurnFinishedEvent<Bridge> = {
	subjectKey: string;
	threadId: string;
	message: AgentChatMessage;
	bridge: Bridge;
};

export type SessionFinishedEvent<Bridge> = {
	subjectKey: string;
	threadId: string;
	bridge: Bridge | null;
	/** Null when the turn completed (or was stopped) without error. */
	error: string | null;
	aborted: boolean;
};

export type CommittedAuthoredTurn = Extract<
	AuthoredTurnCommit,
	{ status: "committed" }
>;
export type ConflictedAuthoredTurn = Extract<
	AuthoredTurnCommit,
	{ status: "conflicted" }
>;

export class AuthoredTurnConflictError extends Error {
	readonly preparedRevisionId: string;
	readonly conflicts: ConflictedAuthoredTurn["conflicts"];
	readonly refreshError: unknown | null;

	constructor(
		result: ConflictedAuthoredTurn,
		refreshError: unknown | null = null,
	) {
		const refreshDetail = refreshError
			? ` Refreshing current state also failed: ${refreshError instanceof Error ? refreshError.message : String(refreshError)}`
			: "";
		super(
			`The agent's changes conflicted with newer edits (${result.conflicts.length} conflict${result.conflicts.length === 1 ? "" : "s"}) and were kept for resolution.${refreshDetail}`,
		);
		this.name = "AuthoredTurnConflictError";
		this.preparedRevisionId = result.preparedRevisionId;
		this.conflicts = result.conflicts;
		this.refreshError = refreshError;
	}
}

export type AuthoredStateAppliedEvent<Bridge> = {
	subjectKey: string;
	threadId: string;
	source: "turn" | "recovery" | "restore" | "subagent";
	result:
		| CommittedAuthoredTurn
		| AuthoredRestoreResult
		| AppliedAuthoredSubagent;
	bridge: Bridge;
};

export type AuthoredStateRefreshEvent<Bridge> = {
	subjectKey: string;
	threadId: string;
	source: "conflict" | "projection_failure" | "recovery_conflict";
	bridge: Bridge;
};

export type AuthoredStateCheckpointEvent<Bridge> = {
	subjectKey: string;
	threadId: string;
	bridge: Bridge;
};

export type CapturedAuthoredState = {
	graph?: Graph;
};

export type AgentChatSpec<Bridge> = {
	/** Persisted on the thread; scopes thread lookup. */
	agentKind: AgentKind;
	subjectKind: SubjectKind;
	/** Fail closed before thread lookup/creation when an agent requires immutable
	 * scope bindings (track agents require both venue and score). */
	validateThreadScope?: (scope: {
		subjectKey: string;
		init: ThreadInit;
		bridge: Bridge | null;
	}) => void;
	/** Build the language model, or return null if the agent isn't configured
	 * yet (e.g. missing API key) — surfaced to the user as an error. */
	createModel: (modelId?: string) => PiAgentModel | null;
	/** Message shown when createModel returns null. */
	notConfiguredMessage?: string;
	/** Build the tool set for one turn. Constructed per turn so tools can close
	 * over the turn's abort signal and thread id. */
	buildTools: (args: BuildToolsArgs<Bridge>) => ToolSet;
	/** Rebuild the parent's domain tools against a child's isolated authored
	 * state. The manager adds recursive delegation tools after this hook. */
	buildSubagentTools?: (args: BuildSubagentToolsArgs<Bridge>) => ToolSet;
	/** System prompt for a turn; may read the live bridge for context. */
	buildSystem: (bridge: Bridge) => string;
	/** Tool-run display vocabulary for the shared renderer. */
	vocab: ToolVocab;
	/** Reasoning effort hint for providers that support it. */
	reasoningEffort?: "low" | "medium" | "high";
	/** Product agent definitions layered over the generic bundled default.
	 * Later definitions with the same name override earlier ones. */
	subagentDefinitions?: AgentDefinition[];
	/** Called once when a turn begins, before streaming. Snapshot the world into
	 * a working copy the tools mutate (so edits don't race UI state). */
	onTurnStart?: (bridge: Bridge) => void;
	/** Called synchronously when streaming ends. Pattern agents clone their exact
	 * graph here; track agents omit the hook because their tools already write
	 * through the authoritative backend. */
	captureAuthoredState?: (
		event: TurnFinishedEvent<Bridge>,
	) => CapturedAuthoredState;
	/** Apply the authoritative projection returned by turn finalization,
	 * hydration recovery, or an explicit restore. */
	applyAuthoredState: (
		event: AuthoredStateAppliedEvent<Bridge>,
	) => void | Promise<void>;
	/** Reload the current authoritative projection after a merge conflict. The
	 * conflicted branch is never applied to the editor. */
	refreshAuthoredState: (
		event: AuthoredStateRefreshEvent<Bridge>,
	) => void | Promise<void>;
	/** Persist editor state that may still exist only in memory before restore
	 * replaces the live projection. Omit when every editor mutation already
	 * writes through the authoritative backend. */
	checkpointAuthoredState?: (
		event: AuthoredStateCheckpointEvent<Bridge>,
	) => void | Promise<void>;
};

export type AgentSession = {
	/** Null until the durable thread has been resolved. */
	threadId: string | null;
	messages: AgentChatMessage[];
	streaming: boolean;
	error: string | null;
	/** True once the thread is resolved and its history hydrated. */
	ready: boolean;
	/** The current turn is draining and another conversation is loading. */
	switching: boolean;
	/** A historical authored revision is being projected and applied. */
	restoring: boolean;
	/** Every conversation in this exact account/subject/venue/score scope. */
	threads: AgentThread[];
	/** Live and terminal child runs owned by this conversation. */
	subagents: import("./subagent-state").SubagentState[];
	send: (text: string) => Promise<void>;
	/** Inject guidance into the active Pi run at its next tool boundary. */
	steer: (text: string) => void;
	stop: () => void;
	newChat: () => Promise<void>;
	openChat: (threadId: string) => Promise<void>;
	refreshChats: () => Promise<void>;
	restoreRevision: (
		targetRevisionId: string,
		mode: AuthoredRestoreMode,
	) => Promise<void>;
};

export type AgentChat<Bridge> = {
	/** Register/refresh the live bridge for an exact subject scope (call from an
	 * effect). The cleanup only removes this registration, so an older effect
	 * cannot tear down a newer bridge for the same scope. */
	registerBridge: (
		subjectKey: string,
		bridge: Bridge,
		init?: ThreadInit,
	) => () => void;
	getBridge: (subjectKey: string, init?: ThreadInit) => Bridge | null;
	/** Resolve (creating if needed) the durable thread for a subject. */
	resolveThreadFor: (subjectKey: string, init?: ThreadInit) => Promise<string>;
	newThreadFor: (subjectKey: string, init?: ThreadInit) => Promise<string>;
	listThreadsFor: (
		subjectKey: string,
		init?: ThreadInit,
	) => Promise<AgentThread[]>;
	openThreadFor: (
		subjectKey: string,
		threadId: string,
		init?: ThreadInit,
	) => Promise<void>;
	/** Restore authored state through the currently active durable thread. */
	restoreStateFor: (
		subjectKey: string,
		targetRevisionId: string,
		mode: AuthoredRestoreMode,
		init?: ThreadInit,
	) => Promise<void>;
	/** Send outside React (background batches). Resolves when the turn ends. */
	send: (subjectKey: string, text: string, init?: ThreadInit) => Promise<void>;
	/** Inject guidance into the currently running turn. */
	steer: (subjectKey: string, text: string, init?: ThreadInit) => void;
	/** Subscribe to a session and get its full conversation lifecycle. */
	useSession: (subjectKey: string | null, init?: ThreadInit) => AgentSession;
	/** Fires whenever any turn of this agent finishes — success, error, or
	 * stop. Background drivers use it to surface completion. */
	onSessionFinished: (
		listener: (event: SessionFinishedEvent<Bridge>) => void,
	) => () => void;
	vocab: ToolVocab;
};

/**
 * Conversations under different principals or persisted scopes must never
 * share a kernel or history. `principalId` partitions only the in-memory
 * frontend cache; the backend independently derives and checks the actual
 * owner. Title is deliberately absent: it is display metadata, while venue and
 * score identify the state the thread is allowed to mutate.
 */
function threadScopeKey(subjectKey: string, init?: ThreadInit): string {
	const principal = init?.principalId ?? null;
	const implementation = init?.implementationId ?? null;
	const venue = init?.venueId ?? null;
	const score = init?.scoreId ?? null;
	return JSON.stringify([principal, subjectKey, implementation, venue, score]);
}

type AuthoredSubagentContext = {
	workspace: PreparedAuthoredSubagent;
};

function withAuthoredMergeResult(
	childResult: string,
	finalization: AuthoredSubagentFinalization,
): string {
	if (finalization.status === "conflicted") {
		return `${childResult}\n\n<authored_merge status="conflicted" proposal_revision_id="${finalization.proposalRevisionId}">\n${JSON.stringify({ conflicts: finalization.conflicts })}\n</authored_merge>`;
	}
	return `${childResult}\n\n<authored_merge status="${finalization.status}" revision_id="${finalization.revisionId}"/>`;
}

function guardToolExecution(
	tools: ToolSet,
	ensureReady: () => Promise<void>,
): ToolSet {
	return Object.fromEntries(
		Object.entries(tools).map(([name, guardedTool]) => {
			const execute = guardedTool.execute;
			if (!execute) return [name, guardedTool];
			const run = execute as (
				input: unknown,
				options: unknown,
			) => unknown | Promise<unknown>;
			return [
				name,
				{
					...guardedTool,
					execute: async (input: unknown, options: unknown) => {
						await ensureReady();
						return run(input, options);
					},
				},
			];
		}),
	) as ToolSet;
}

function serializeWorkspaceToolExecution(
	tools: ToolSet,
	runWorkspaceOperation: PreparedAuthoredSubagent["runWorkspaceOperation"],
): ToolSet {
	return Object.fromEntries(
		Object.entries(tools).map(([name, workspaceTool]) => {
			const execute = workspaceTool.execute;
			if (!execute) return [name, workspaceTool];
			const run = execute as (
				input: unknown,
				options: unknown,
			) => unknown | Promise<unknown>;
			return [
				name,
				{
					...workspaceTool,
					execute: (input: unknown, options: unknown) =>
						runWorkspaceOperation(() => run(input, options)),
				},
			];
		}),
	) as ToolSet;
}

export function createAgentChat<Bridge>(
	spec: AgentChatSpec<Bridge>,
): AgentChat<Bridge> {
	type Session = {
		subjectKey: string;
		/** Exact principal + subject + venue + score key captured on resolution. */
		scopeKey: string;
		thread: AgentThread;
		threadId: string;
		messages: AgentChatMessage[];
		agent: Agent | null;
		unsubscribeAgent: () => void;
		streaming: boolean;
		turnError: string | null;
		turnAbortController: AbortController | null;
		ensureProjectionReady: () => Promise<void>;
		subagentManager: SubagentManager<AuthoredSubagentContext>;
		subagents: SubagentSnapshot[];
		unsubscribeSubagents: () => void;
		authoredSubagentContexts: Set<AuthoredSubagentContext>;
		pendingSubagentSnapshots: Map<string, SubagentSnapshot>;
		flushingSubagents: Promise<void>;
		baseline: PersistedMessage[];
		/** Serializes writes so a turn's persist can't overtake the previous. */
		persisting: Promise<void>;
		/** Every send through this session, including its terminal persistence. */
		activeTurns: Set<Promise<void>>;
		/** A completed turn whose transcript + authored state must cross the
		 * durable boundary together before another operation may overtake it. */
		pendingFinalization: {
			message: AgentChatMessage;
			messages: AgentChatMessage[];
			isAbort: boolean;
			isError: boolean;
			capturedState: CapturedAuthoredState;
			captureError: unknown | null;
			prepared: PreparedAuthoredTurn | null;
			result: AuthoredTurnCommit | null;
		} | null;
		/** One user-requested restore operation. Its stable operation id and
		 * returned projection survive response loss or a failed editor refresh. */
		pendingRestore: {
			targetRevisionId: string;
			mode: AuthoredRestoreMode;
			operationId: string;
			result: AuthoredRestoreResult | null;
			applied: boolean;
		} | null;
		/** Serialized finalization attempts. A failed attempt retains its exact
		 * capture, prepared branch commit, and returned projection, so each retry
		 * resumes the earliest incomplete phase. */
		finalizing: Promise<void>;
	};
	type ScopeState = {
		intent: number;
		switching: boolean;
		restoring: boolean;
		error: string | null;
		threads: AgentThread[];
	};

	const bridges = new Map<
		string,
		Array<{ bridge: Bridge; registration: symbol }>
	>();
	const bridgeWaiters = new Map<string, Set<(bridge: Bridge) => void>>();
	/** Only active hydrated chats live here; inactive histories stay in SQLite. */
	const chats = new Map<string, Session>();
	const activeThreadByScope = new Map<string, string>();
	const resolving = new Map<string, Promise<Session>>();
	const transitionTails = new Map<string, Promise<void>>();
	const scopeStates = new Map<string, ScopeState>();
	const watchers = new Map<string, Set<() => void>>();
	const finishedListeners = new Set<
		(event: SessionFinishedEvent<Bridge>) => void
	>();
	const validateThreadScope = (
		subjectKey: string,
		init?: ThreadInit,
		bridge: Bridge | null = null,
	): void => {
		spec.validateThreadScope?.({
			subjectKey,
			init: init ?? { principalId: null },
			bridge,
		});
	};

	const getBridgeForScope = (scopeKey: string): Bridge | null => {
		const registered = bridges.get(scopeKey);
		return registered?.[registered.length - 1]?.bridge ?? null;
	};
	const getBridge = (subjectKey: string, init?: ThreadInit): Bridge | null =>
		getBridgeForScope(threadScopeKey(subjectKey, init));
	const registerBridge = (
		subjectKey: string,
		bridge: Bridge,
		init?: ThreadInit,
	) => {
		validateThreadScope(subjectKey, init, bridge);
		const scopeKey = threadScopeKey(subjectKey, init);
		const registration = Symbol(scopeKey);
		bridges.set(scopeKey, [
			...(bridges.get(scopeKey) ?? []),
			{ bridge, registration },
		]);
		const waiters = bridgeWaiters.get(scopeKey);
		if (waiters) {
			bridgeWaiters.delete(scopeKey);
			for (const resolve of waiters) resolve(bridge);
		}
		return () => {
			const remaining = (bridges.get(scopeKey) ?? []).filter(
				(entry) => entry.registration !== registration,
			);
			if (remaining.length === 0) {
				bridges.delete(scopeKey);
			} else {
				bridges.set(scopeKey, remaining);
			}
		};
	};
	const waitForBridge = (scopeKey: string): Promise<Bridge> => {
		const bridge = getBridgeForScope(scopeKey);
		if (bridge) return Promise.resolve(bridge);
		return new Promise((resolve) => {
			const waiters = bridgeWaiters.get(scopeKey) ?? new Set();
			waiters.add(resolve);
			bridgeWaiters.set(scopeKey, waiters);
		});
	};

	const currentSession = (
		subjectKey: string | null,
		init?: ThreadInit,
	): Session | null => {
		if (!subjectKey) return null;
		const threadId = activeThreadByScope.get(threadScopeKey(subjectKey, init));
		return threadId ? (chats.get(threadId) ?? null) : null;
	};
	const stateFor = (scopeKey: string): ScopeState => {
		let state = scopeStates.get(scopeKey);
		if (!state) {
			state = {
				intent: 0,
				switching: false,
				restoring: false,
				error: null,
				threads: [],
			};
			scopeStates.set(scopeKey, state);
		}
		return state;
	};

	const notify = (scopeKey: string) => {
		for (const w of watchers.get(scopeKey) ?? []) w();
	};
	const reportError = (scopeKey: string, error: unknown) => {
		stateFor(scopeKey).error =
			error instanceof Error ? error.message : String(error);
		notify(scopeKey);
	};

	const persist = (
		session: Session,
		messages: AgentChatMessage[],
	): Promise<void> => {
		const next = session.persisting
			// A failed write is visible to its caller, but must not poison the
			// serialization tail: a later strict drain gets a real retry.
			.catch(() => undefined)
			.then(async () => {
				session.baseline = await appendThreadMessages(
					session.threadId,
					session.baseline,
					messages,
				);
			});
		session.persisting = next;
		return next;
	};

	const flushSubagentSnapshots = (session: Session): Promise<void> => {
		const attempt = session.flushingSubagents
			.catch(() => undefined)
			.then(async () => {
				if (
					session.activeTurns.size > 0 ||
					session.pendingFinalization !== null ||
					session.pendingSubagentSnapshots.size === 0
				) {
					return;
				}
				const snapshots = [...session.pendingSubagentSnapshots.values()].map(
					(snapshot) => structuredClone(snapshot),
				);
				session.pendingSubagentSnapshots.clear();
				const milestones: AgentChatMessage[] = snapshots.map((snapshot) => ({
					id: `subagent:${snapshot.id}:${snapshot.status}`,
					role: "assistant",
					parts: [{ type: "data-subagent", data: snapshot }],
				}));
				const previous = session.messages;
				const next = [...previous, ...milestones];
				session.messages = next;
				try {
					await persist(session, next);
				} catch (error) {
					session.messages = previous;
					for (const snapshot of snapshots) {
						session.pendingSubagentSnapshots.set(snapshot.id, snapshot);
					}
					throw error;
				}
			});
		session.flushingSubagents = attempt;
		return attempt;
	};

	const applyAuthoredProjection = async (
		session: Pick<Session, "subjectKey" | "scopeKey" | "threadId">,
		result: AuthoredStateAppliedEvent<Bridge>["result"],
		source: AuthoredStateAppliedEvent<Bridge>["source"],
		waitForMissingBridge = false,
	): Promise<void> => {
		const bridge = waitForMissingBridge
			? await waitForBridge(session.scopeKey)
			: getBridgeForScope(session.scopeKey);
		if (!bridge) {
			throw new Error(
				"Editor is unavailable; authoritative state was not applied.",
			);
		}
		await spec.applyAuthoredState({
			subjectKey: session.subjectKey,
			threadId: session.threadId,
			source,
			result,
			bridge,
		});
	};

	const refreshAuthoredProjection = async (
		session: Pick<Session, "subjectKey" | "scopeKey" | "threadId">,
		source: AuthoredStateRefreshEvent<Bridge>["source"],
		waitForMissingBridge = false,
	): Promise<void> => {
		const bridge = waitForMissingBridge
			? await waitForBridge(session.scopeKey)
			: getBridgeForScope(session.scopeKey);
		if (!bridge) {
			throw new Error(
				"Editor is unavailable; current authored state was not refreshed.",
			);
		}
		await spec.refreshAuthoredState({
			subjectKey: session.subjectKey,
			threadId: session.threadId,
			source,
			bridge,
		});
	};

	const completeTurn = (
		session: Session,
		pending: NonNullable<Session["pendingFinalization"]>,
		errorOverride: string | null = null,
	): void => {
		// Clear before notifying observers: listeners may synchronously start
		// another operation, which must not retry an already-terminal turn.
		if (session.pendingFinalization === pending) {
			session.pendingFinalization = null;
		}
		const event: SessionFinishedEvent<Bridge> = {
			subjectKey: session.subjectKey,
			threadId: session.threadId,
			bridge: getBridgeForScope(session.scopeKey),
			error:
				errorOverride ??
				(pending.isError ? (session.turnError ?? "Agent error") : null),
			aborted: pending.isAbort,
		};
		for (const listener of finishedListeners) {
			try {
				listener(event);
			} catch (err) {
				console.error("[agent-chat] session-finished listener threw:", err);
			}
		}
	};

	const finalizePending = (session: Session): Promise<void> => {
		if (!session.pendingFinalization) return Promise.resolve();
		const attempt = session.finalizing
			.catch(() => undefined)
			.then(async () => {
				const pending = session.pendingFinalization;
				if (!pending) return;
				if (pending.captureError) throw pending.captureError;
				if (!pending.prepared) {
					pending.prepared = await prepareAuthoredTurn({
						threadId: session.threadId,
						assistantMessageId: pending.message.id,
						graph: pending.capturedState.graph ?? null,
					});
				}

				// The assistant transcript becomes durable only after the exact authored
				// state has an immutable prepared revision. If the process dies after this
				// write, hydration recovery can finish the recorded association.
				await persist(session, pending.messages);
				if (!pending.result) {
					pending.result = await finalizeAuthoredTurn({
						threadId: session.threadId,
						assistantMessageId: pending.message.id,
						preparedRevisionId: pending.prepared.preparedRevisionId,
					});
				}
				const result = pending.result;
				if (result.status === "conflicted") {
					let refreshError: unknown | null = null;
					try {
						await refreshAuthoredProjection(session, "conflict");
					} catch (error) {
						refreshError = error;
					}
					const error = new AuthoredTurnConflictError(result, refreshError);
					completeTurn(session, pending, error.message);
					throw error;
				}
				await applyAuthoredProjection(session, result, "turn");
				completeTurn(session, pending);
			});
		session.finalizing = attempt;
		return attempt;
	};

	const completePendingRestore = async (
		session: Session,
	): Promise<AuthoredRestoreResult | null> => {
		const pending = session.pendingRestore;
		if (!pending) return null;
		if (!pending.result) {
			if (spec.checkpointAuthoredState) {
				const bridge = getBridgeForScope(session.scopeKey);
				if (!bridge) {
					throw new Error(
						"Editor is unavailable; current authored state was not checkpointed.",
					);
				}
				await spec.checkpointAuthoredState({
					subjectKey: session.subjectKey,
					threadId: session.threadId,
					bridge,
				});
			}
			pending.result = await restoreAuthoredState({
				threadId: session.threadId,
				targetRevisionId: pending.targetRevisionId,
				operationId: pending.operationId,
				mode: pending.mode,
			});
		}
		const result = pending.result;
		if (chats.get(session.threadId) !== session) {
			throw new Error(
				"Conversation changed before the restored state could be applied.",
			);
		}
		if (!pending.applied) {
			await applyAuthoredProjection(session, result, "restore");
			pending.applied = true;
		}
		return result;
	};

	const makeSession = (
		subjectKey: string,
		scopeKey: string,
		thread: AgentThread,
		messages: AgentChatMessage[],
		baseline: PersistedMessage[],
	): Session => {
		const threadId = thread.id;
		let session: Session;
		const authoredSubagentContexts = new Set<AuthoredSubagentContext>();
		let subagentCheckpointTail = Promise.resolve();
		let projectionRecoveryPending = false;
		const sessionScope = { subjectKey, scopeKey, threadId };
		const ensureProjectionReady = async (): Promise<void> => {
			if (!projectionRecoveryPending) return;
			await refreshAuthoredProjection(sessionScope, "projection_failure");
			projectionRecoveryPending = false;
		};
		const checkpointForSubagent = (): Promise<void> => {
			if (!spec.checkpointAuthoredState) return Promise.resolve();
			const checkpoint = subagentCheckpointTail
				.catch(() => undefined)
				.then(async () => {
					await ensureProjectionReady();
					const bridge = getBridgeForScope(scopeKey);
					if (!bridge) {
						throw new Error(
							"Editor is unavailable; current state was not checkpointed before delegation.",
						);
					}
					await spec.checkpointAuthoredState?.({
						subjectKey,
						threadId,
						bridge,
					});
				});
			subagentCheckpointTail = checkpoint.then(
				() => undefined,
				() => undefined,
			);
			return checkpoint;
		};
		const supervisor = new AuthoredSubagentSupervisor(
			threadId,
			async (result) => {
				try {
					await applyAuthoredProjection(sessionScope, result, "subagent");
					projectionRecoveryPending = false;
				} catch (applyError) {
					try {
						await refreshAuthoredProjection(sessionScope, "projection_failure");
						projectionRecoveryPending = false;
					} catch (refreshError) {
						projectionRecoveryPending = true;
						throw new AggregateError(
							[applyError, refreshError],
							"Failed to apply or refresh the authoritative subagent projection.",
						);
					}
				}
			},
			checkpointForSubagent,
		);
		const runner = createPiSubagentRunner({
			createModel: (modelId) => {
				const model = spec.createModel(modelId);
				if (!model) {
					throw new Error(spec.notConfiguredMessage ?? "Agent not configured.");
				}
				return model;
			},
		});
		const subagentManager = new SubagentManager<AuthoredSubagentContext>({
			runner,
			agentLoader: new AgentLoader({ definitions: spec.subagentDefinitions }),
			environment:
				"Application: Luma desktop\nAuthored state: isolated child revision\nEdits: parent-equivalent tools target only this revision; the supervisor merges it after completion",
			prepareSpawn: async ({
				id,
				parentSubagentId,
				turnMessageId,
				abortSignal,
			}) => {
				await checkpointForSubagent();
				const workspace = await supervisor.prepare(id, parentSubagentId);
				try {
					if (!turnMessageId) {
						throw new Error(
							"Subagent domain tools require the durable root turn message.",
						);
					}
					if (!spec.buildSubagentTools) {
						throw new Error(
							"This agent has no isolated subagent domain-tool implementation.",
						);
					}
					const buildArgs: BuildToolsArgs<Bridge> = {
						getBridge: () => getBridgeForScope(scopeKey),
						threadId,
						turnMessageId,
						abortSignal,
					};
					const domainTools = spec.buildSubagentTools({
						...buildArgs,
						subagentId: id,
						workspaceId: workspace.workspaceId,
						initialDocument: workspace.initialDocument,
						bindWorkspaceDocument: workspace.bindDocumentSink,
					});
					assertSameDomainToolSurface(spec.buildTools(buildArgs), domainTools);
					const tools = serializeWorkspaceToolExecution(
						domainTools,
						workspace.runWorkspaceOperation,
					);
					const context: AuthoredSubagentContext = { workspace };
					authoredSubagentContexts.add(context);
					return {
						tools,
						context,
						finalize: async ({ outcome }) => {
							if (outcome.status !== "completed") return;
							let finalization: AuthoredSubagentFinalization;
							try {
								finalization = await workspace.finalize(outcome.result);
							} catch {
								// Workspace finalization is phase-resumable and uses stable
								// operation ids. One immediate replay recovers a lost IPC response
								// without duplicating a commit or merge.
								finalization = await workspace.finalize(outcome.result);
							}
							authoredSubagentContexts.delete(context);
							return withAuthoredMergeResult(outcome.result, finalization);
						},
						cleanup: async () => {
							await workspace.discard();
							authoredSubagentContexts.delete(context);
						},
					};
				} catch (error) {
					await workspace.discard();
					throw error;
				}
			},
		});
		session = {
			subjectKey,
			scopeKey,
			thread,
			threadId,
			messages,
			agent: null,
			unsubscribeAgent: () => undefined,
			streaming: false,
			turnError: null,
			turnAbortController: null,
			ensureProjectionReady,
			subagentManager,
			subagents: [],
			unsubscribeSubagents: () => undefined,
			authoredSubagentContexts,
			pendingSubagentSnapshots: new Map(),
			flushingSubagents: Promise.resolve(),
			baseline,
			persisting: Promise.resolve(),
			activeTurns: new Set(),
			pendingFinalization: null,
			pendingRestore: null,
			finalizing: Promise.resolve(),
		};
		session.unsubscribeSubagents = subagentManager.subscribe((snapshots) => {
			session.subagents = snapshots;
			for (const snapshot of snapshots) {
				if (
					snapshot.status !== "running" &&
					snapshot.finishedAt !== undefined
				) {
					session.pendingSubagentSnapshots.set(snapshot.id, snapshot);
				}
			}
			notify(scopeKey);
			void flushSubagentSnapshots(session).catch((error) =>
				reportError(scopeKey, error),
			);
		});
		return session;
	};

	class SupersededThreadTransitionError extends Error {
		constructor() {
			super("Conversation switch was superseded by a newer request.");
			this.name = "SupersededThreadTransitionError";
		}
	}

	const refreshThreads = async (
		subjectKey: string,
		init?: ThreadInit,
	): Promise<AgentThread[]> => {
		validateThreadScope(subjectKey, init);
		const scopeKey = threadScopeKey(subjectKey, init);
		const scope = normalizeThreadScope(
			spec.agentKind,
			spec.subjectKind,
			subjectKey,
			init,
		);
		const threads = await listScopedThreads(scope);
		stateFor(scopeKey).threads = threads;
		notify(scopeKey);
		return threads;
	};

	const activate = (
		subjectKey: string,
		init: ThreadInit | undefined,
		loadTarget: () => Promise<AgentThread>,
	): Promise<Session> => {
		validateThreadScope(subjectKey, init);
		const scopeKey = threadScopeKey(subjectKey, init);
		const scope = normalizeThreadScope(
			spec.agentKind,
			spec.subjectKind,
			subjectKey,
			init,
		);
		const state = stateFor(scopeKey);
		const intent = ++state.intent;
		state.switching = true;
		state.error = null;
		notify(scopeKey);

		const previousTail = transitionTails.get(scopeKey) ?? Promise.resolve();
		const operation = previousTail
			.catch(() => undefined)
			.then(async () => {
				if (stateFor(scopeKey).intent !== intent) {
					throw new SupersededThreadTransitionError();
				}

				const previous = currentSession(subjectKey, init);
				if (previous) {
					previous.agent?.abort();
					previous.agent?.clearAllQueues();
					await Promise.allSettled([...previous.activeTurns]);
					await previous.subagentManager.dispose();
					await Promise.allSettled(
						[...previous.authoredSubagentContexts].map((context) =>
							context.workspace.discard(),
						),
					);
					previous.authoredSubagentContexts.clear();
					// A failed authored-state commit remains pending even after its
					// original send rejects. Switching retries it by assistant message
					// id before the old session can be evicted.
					await finalizePending(previous);
					await completePendingRestore(previous);
					await flushSubagentSnapshots(previous);
					previous.unsubscribeAgent();
					previous.unsubscribeSubagents();
					// This is the strict boundary: activation cannot overtake an
					// unpersisted tail from the conversation being left.
					await persist(previous, previous.messages);
				}

				if (stateFor(scopeKey).intent !== intent) {
					throw new SupersededThreadTransitionError();
				}
				const thread = await loadTarget();
				if (!threadMatchesScope(thread, scope)) {
					throw new Error(
						`Agent thread '${thread.id}' does not belong to this conversation scope.`,
					);
				}
				const { messages, baseline } = await loadThreadMessages(thread.id);
				if (stateFor(scopeKey).intent !== intent) {
					throw new SupersededThreadTransitionError();
				}
				const recovered = await recoverAuthoredTurns(thread.id);
				const recoveredCommits = recovered.filter(
					(result): result is CommittedAuthoredTurn =>
						result.status === "committed",
				);
				const recoveredConflicts = recovered.filter(
					(result): result is ConflictedAuthoredTurn =>
						result.status === "conflicted",
				);
				const lastRecoveredCommit = recoveredCommits.at(-1);
				const recoveredSession = {
					subjectKey,
					scopeKey,
					threadId: thread.id,
				};
				if (lastRecoveredCommit) {
					await applyAuthoredProjection(
						recoveredSession,
						lastRecoveredCommit,
						"recovery",
						true,
					);
				}
				let recoveryError: string | null = null;
				if (recoveredConflicts.length > 0) {
					let refreshDetail = "";
					try {
						await refreshAuthoredProjection(
							recoveredSession,
							"recovery_conflict",
							true,
						);
					} catch (error) {
						refreshDetail = ` Refreshing current state also failed: ${error instanceof Error ? error.message : String(error)}`;
					}
					const conflictCount = recoveredConflicts.reduce(
						(count, result) => count + result.conflicts.length,
						0,
					);
					recoveryError = `Recovered authored state has ${conflictCount} unresolved merge conflict${conflictCount === 1 ? "" : "s"}; the conflicting branches were kept for resolution.${refreshDetail}`;
				}
				if (stateFor(scopeKey).intent !== intent) {
					throw new SupersededThreadTransitionError();
				}

				const next = makeSession(
					subjectKey,
					scopeKey,
					thread,
					messages,
					baseline,
				);
				chats.set(thread.id, next);
				activeThreadByScope.set(scopeKey, thread.id);
				if (previous && previous.threadId !== thread.id) {
					chats.delete(previous.threadId);
				}
				const currentState = stateFor(scopeKey);
				currentState.threads = [
					thread,
					...currentState.threads.filter(
						(candidate) => candidate.id !== thread.id,
					),
				];
				currentState.error = recoveryError;
				currentState.switching = false;
				notify(scopeKey);
				return next;
			});

		transitionTails.set(
			scopeKey,
			operation.then(
				() => undefined,
				() => undefined,
			),
		);
		void operation.then(
			() => {
				void refreshThreads(subjectKey, init).catch((error) =>
					reportError(scopeKey, error),
				);
			},
			(error) => {
				if (
					!(error instanceof SupersededThreadTransitionError) &&
					stateFor(scopeKey).intent === intent
				) {
					stateFor(scopeKey).switching = false;
					reportError(scopeKey, error);
				}
			},
		);
		return operation;
	};

	const ensureSession = (
		subjectKey: string,
		init?: ThreadInit,
	): Promise<Session> => {
		const scopeKey = threadScopeKey(subjectKey, init);
		const existing = currentSession(subjectKey, init);
		if (existing) return Promise.resolve(existing);
		const inFlight = resolving.get(scopeKey);
		if (inFlight) return inFlight;

		const promise = activate(subjectKey, init, () =>
			resolveThread(spec.agentKind, spec.subjectKind, subjectKey, init),
		);
		resolving.set(scopeKey, promise);
		void promise.then(
			() => {
				if (resolving.get(scopeKey) === promise) resolving.delete(scopeKey);
			},
			() => {
				if (resolving.get(scopeKey) === promise) resolving.delete(scopeKey);
			},
		);
		return promise;
	};

	const configureAgent = async (
		session: Session,
		userMessage: AgentChatMessage,
		abortSignal: AbortSignal,
	): Promise<Agent> => {
		const bridge = getBridgeForScope(session.scopeKey);
		if (!bridge) throw new Error("Editor not ready.");
		await session.ensureProjectionReady();
		const runtime = spec.createModel();
		if (!runtime) {
			throw new Error(spec.notConfiguredMessage ?? "Agent not configured.");
		}
		spec.onTurnStart?.(bridge);
		const systemPrompt = spec.buildSystem(bridge);
		const parentTools = spec.buildTools({
			getBridge: () => getBridgeForScope(session.scopeKey),
			abortSignal,
			threadId: session.threadId,
			turnMessageId: userMessage.id,
		});
		const tools = guardToolExecution(
			{
				...parentTools,
				...createSubagentTools(session.subagentManager, {
					getParentSystemPrompt: () => systemPrompt,
					turnMessageId: userMessage.id,
				}),
			},
			session.ensureProjectionReady,
		);

		if (!session.agent) {
			const agent = await createPiAgent({
				runtime,
				systemPrompt,
				tools,
				// `send` has already inserted the user bubble in frontend state.
				// prompt() adds that same message to Pi's transcript, so seed only
				// the history that precedes it.
				messages: session.messages.filter(
					(message) => message.id !== userMessage.id,
				),
				thinkingLevel: spec.reasoningEffort,
				sessionId: session.threadId,
			});
			session.agent = agent;
			session.unsubscribeAgent = agent.subscribe((event: AgentEvent) => {
				if (chats.get(session.threadId) !== session) return;
				session.messages = applyAgentEvent(session.messages, event);
				if (
					event.type === "message_end" &&
					event.message.role === "assistant"
				) {
					session.turnError =
						event.message.stopReason === "error"
							? (event.message.errorMessage ?? "Model turn ended in error.")
							: null;
				}
				notify(session.scopeKey);
			});
			return agent;
		}

		session.agent.state.systemPrompt = systemPrompt;
		session.agent.state.model = runtime.model;
		session.agent.state.thinkingLevel = (spec.reasoningEffort ??
			"off") as typeof session.agent.state.thinkingLevel;
		session.agent.state.tools = toPiTools(tools);
		session.agent.streamFn = runtime.streamFn;
		return session.agent;
	};

	const stageTurnFinalization = (
		session: Session,
		message: AgentChatMessage,
		isAbort: boolean,
		isError: boolean,
	): void => {
		const bridge = getBridgeForScope(session.scopeKey);
		let capturedState: CapturedAuthoredState = {};
		let captureError: unknown | null = null;
		if (spec.captureAuthoredState) {
			if (!bridge) {
				captureError = new Error(
					"Editor is unavailable; authored state was not recorded.",
				);
			} else {
				try {
					capturedState = structuredClone(
						spec.captureAuthoredState({
							subjectKey: session.subjectKey,
							threadId: session.threadId,
							message,
							bridge,
						}),
					);
				} catch (error) {
					captureError = error;
				}
			}
		}
		session.pendingFinalization = {
			message,
			messages: [...session.messages],
			isAbort,
			isError,
			capturedState,
			captureError,
			prepared: null,
			result: null,
		};
	};

	const send = async (
		subjectKey: string,
		text: string,
		init?: ThreadInit,
	): Promise<void> => {
		const prompt = text.trim();
		if (!prompt) return;
		const session = await ensureSession(subjectKey, init);
		if (
			stateFor(session.scopeKey).switching ||
			stateFor(session.scopeKey).restoring ||
			session.activeTurns.size > 0
		) {
			throw new Error(
				"Wait for the current turn to finish before sending again.",
			);
		}
		stateFor(session.scopeKey).error = null;
		notify(session.scopeKey);
		const userMessage = userChatMessage(prompt);
		const withUser = [...session.messages, userMessage];
		const completion = (async () => {
			// Do not let a new prompt overtake a completed assistant response whose
			// state commit failed. The backend commit is idempotent by message id.
			await finalizePending(session);
			await completePendingRestore(session);
			// The prompt is durable before any remote model call begins. Including
			// this write in activeTurns also lets a concurrent switch drain it.
			await persist(session, withUser);
			if (
				currentSession(subjectKey, init) !== session ||
				stateFor(session.scopeKey).switching
			) {
				throw new Error("Conversation changed before the turn began.");
			}
			session.messages = withUser;
			session.turnError = null;
			const abortController = new AbortController();
			session.turnAbortController = abortController;
			const agent = await configureAgent(
				session,
				userMessage,
				abortController.signal,
			);
			session.streaming = true;
			notify(session.scopeKey);
			let runFailure: unknown;
			try {
				await agent.prompt(userAgentMessage(userMessage));
			} catch (error) {
				runFailure = error;
			} finally {
				session.streaming = false;
				session.turnAbortController = null;
				notify(session.scopeKey);
			}
			const finalAgentMessage = [...agent.state.messages]
				.reverse()
				.find((message) => message.role === "assistant");
			const assistantMessage = [...session.messages]
				.reverse()
				.find(
					(message) =>
						message.role === "assistant" &&
						!message.parts.every((part) => part.type === "data-subagent"),
				);
			if (!assistantMessage || !finalAgentMessage) {
				if (runFailure) throw runFailure;
				throw new Error("Pi agent turn ended without an assistant message.");
			}
			const isAbort = finalAgentMessage.stopReason === "aborted";
			const isError = finalAgentMessage.stopReason === "error";
			stageTurnFinalization(session, assistantMessage, isAbort, isError);
			await finalizePending(session);
			await persist(session, session.messages);
			if (runFailure && !isAbort && !isError) throw runFailure;
		})();
		session.activeTurns.add(completion);
		try {
			await completion;
		} catch (error) {
			reportError(session.scopeKey, error);
			throw error;
		} finally {
			session.activeTurns.delete(completion);
			void flushSubagentSnapshots(session).catch((error) =>
				reportError(session.scopeKey, error),
			);
		}
	};

	const steer = (subjectKey: string, text: string, init?: ThreadInit): void => {
		const prompt = text.trim();
		if (!prompt) return;
		const session = currentSession(subjectKey, init);
		if (!session?.agent || !session.streaming || !session.agent.signal) {
			throw new Error("There is no active turn to steer.");
		}
		const scopeState = stateFor(session.scopeKey);
		if (scopeState.switching || scopeState.restoring) {
			throw new Error("Conversation state is changing; try again in a moment.");
		}
		const message = userChatMessage(prompt);
		// Keep queued steering out of the durable transcript until Pi injects it.
		// Otherwise a steer sent before the first assistant event would be stored
		// ahead of the response it is meant to redirect.
		session.agent.steer(userAgentMessage(message));
	};

	const restoreStateFor = async (
		subjectKey: string,
		targetRevisionId: string,
		mode: AuthoredRestoreMode,
		init?: ThreadInit,
	): Promise<void> => {
		if (!targetRevisionId) throw new Error("A revision is required.");
		const session = await ensureSession(subjectKey, init);
		const state = stateFor(session.scopeKey);
		if (state.switching || state.restoring || session.activeTurns.size > 0) {
			throw new Error(
				"Wait for the current operation to finish before restoring.",
			);
		}
		if (
			!session.pendingRestore ||
			session.pendingRestore.targetRevisionId !== targetRevisionId ||
			session.pendingRestore.mode !== mode
		) {
			session.pendingRestore = {
				targetRevisionId,
				mode,
				operationId: crypto.randomUUID(),
				result: null,
				applied: false,
			};
		}
		state.restoring = true;
		state.error = null;
		notify(session.scopeKey);
		const completion = (async () => {
			await finalizePending(session);
			await completePendingRestore(session);
		})();
		session.activeTurns.add(completion);
		try {
			await completion;
		} catch (error) {
			reportError(session.scopeKey, error);
			throw error;
		} finally {
			session.activeTurns.delete(completion);
			state.restoring = false;
			notify(session.scopeKey);
			void flushSubagentSnapshots(session).catch((error) =>
				reportError(session.scopeKey, error),
			);
		}
		const restoreResult = session.pendingRestore?.result;
		if (restoreResult?.forkedThreadId) {
			const forkedThreadId = restoreResult.forkedThreadId;
			await activate(subjectKey, init, async () => {
				const detail = await getThread(forkedThreadId);
				return detail.thread;
			});
		}
		// Keep the exact operation/result until both projection application and
		// optional fork activation succeed. A lost activation response can then
		// retry without creating another restore revision or transcript fork.
		session.pendingRestore = null;
	};

	const useSession: AgentChat<Bridge>["useSession"] = (subjectKey, init) => {
		const scopeKey = subjectKey ? threadScopeKey(subjectKey, init) : null;
		const [, render] = useState(0);
		const initRef = useRef(init);
		initRef.current = init;

		useEffect(() => {
			if (!subjectKey) return;
			let live = true;
			const sync = () => {
				if (live) render((version) => version + 1);
			};
			if (!scopeKey) return;
			const set = watchers.get(scopeKey) ?? new Set<() => void>();
			set.add(sync);
			watchers.set(scopeKey, set);
			sync();
			void ensureSession(subjectKey, initRef.current).catch((error) => {
				if (live && !(error instanceof SupersededThreadTransitionError)) {
					reportError(scopeKey, error);
				}
			});
			return () => {
				live = false;
				set.delete(sync);
			};
		}, [subjectKey, scopeKey]);
		const session = currentSession(subjectKey, init);
		const scopeState = scopeKey ? stateFor(scopeKey) : null;

		const messages = session?.messages ?? [];

		const doSend = useCallback(
			async (text: string) => {
				if (!subjectKey) return;
				await send(subjectKey, text, initRef.current);
			},
			[subjectKey],
		);
		const doSteer = useCallback(
			(text: string) => {
				if (!subjectKey) return;
				steer(subjectKey, text, initRef.current);
			},
			[subjectKey],
		);
		const stop = useCallback(() => {
			session?.turnAbortController?.abort();
			session?.agent?.abort();
			session?.agent?.clearAllQueues();
		}, [session]);

		const newChat = useCallback(async () => {
			if (!subjectKey) return;
			const scope = normalizeThreadScope(
				spec.agentKind,
				spec.subjectKind,
				subjectKey,
				initRef.current,
			);
			await activate(subjectKey, initRef.current, () =>
				createScopedThread(scope, initRef.current?.title ?? null),
			);
		}, [subjectKey]);
		const openChat = useCallback(
			async (threadId: string) => {
				if (!subjectKey || threadId === session?.threadId) return;
				await activate(subjectKey, initRef.current, async () => {
					const detail = await getThread(threadId);
					return detail.thread;
				});
			},
			[subjectKey, session?.threadId],
		);
		const refreshChats = useCallback(async () => {
			if (!subjectKey) return;
			await refreshThreads(subjectKey, initRef.current);
		}, [subjectKey]);
		const restoreRevision = useCallback(
			async (targetRevisionId: string, mode: AuthoredRestoreMode) => {
				if (!subjectKey) return;
				await restoreStateFor(
					subjectKey,
					targetRevisionId,
					mode,
					initRef.current,
				);
			},
			[subjectKey],
		);

		return {
			threadId: session?.threadId ?? null,
			messages,
			streaming: session?.streaming ?? false,
			error: scopeState?.error ?? session?.turnError ?? null,
			ready: session !== null,
			switching: scopeState?.switching ?? false,
			restoring: scopeState?.restoring ?? false,
			threads: scopeState?.threads ?? [],
			subagents: session?.subagents ?? [],
			send: doSend,
			steer: doSteer,
			stop,
			newChat,
			openChat,
			refreshChats,
			restoreRevision,
		};
	};

	return {
		registerBridge,
		getBridge,
		resolveThreadFor: async (subjectKey, init) =>
			(await ensureSession(subjectKey, init)).threadId,
		newThreadFor: async (subjectKey, init) => {
			const scope = normalizeThreadScope(
				spec.agentKind,
				spec.subjectKind,
				subjectKey,
				init,
			);
			return (
				await activate(subjectKey, init, () =>
					createScopedThread(scope, init?.title ?? null),
				)
			).threadId;
		},
		listThreadsFor: refreshThreads,
		openThreadFor: async (subjectKey, threadId, init) => {
			await activate(subjectKey, init, async () => {
				const detail = await getThread(threadId);
				return detail.thread;
			});
		},
		restoreStateFor,
		send,
		steer,
		useSession,
		onSessionFinished: (listener) => {
			finishedListeners.add(listener);
			return () => {
				finishedListeners.delete(listener);
			};
		},
		vocab: spec.vocab,
	};
}
