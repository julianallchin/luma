import { Chat, useChat } from "@ai-sdk/react";
import {
	type ChatTransport,
	convertToModelMessages,
	type JSONValue,
	type LanguageModel,
	type StopCondition,
	stepCountIs,
	streamText,
	type ToolSet,
	type UIMessage,
	type UIMessageChunk,
} from "ai";
import { useCallback, useEffect, useRef, useState } from "react";
import type { AgentThread, Graph } from "@/bindings/schema";
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
	type AgentDefinition,
	AgentLoader,
	createAiSdkSubagentRunner,
	createSubagentTools,
	SubagentManager,
	type SubagentSnapshot,
	type SubagentThinkingLevel,
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
import { smoothUIMessageStream } from "./smooth-ui-message-stream";

/**
 * The base agent-chat encapsulation, built on the AI SDK's `useChat` / `Chat`
 * over durable backend threads.
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

export type TurnFinishedEvent<Bridge> = {
	subjectKey: string;
	threadId: string;
	message: UIMessage;
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
	source: "conflict" | "recovery_conflict";
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
	createModel: (modelId?: string) => LanguageModel | null;
	/** Message shown when createModel returns null. */
	notConfiguredMessage?: string;
	/** Build the tool set for one turn. Constructed per turn so tools can close
	 * over the turn's abort signal and thread id. */
	buildTools: (args: BuildToolsArgs<Bridge>) => ToolSet;
	/** System prompt for a turn; may read the live bridge for context. */
	buildSystem: (bridge: Bridge) => string;
	/** Tool-run display vocabulary for the shared renderer. */
	vocab: ToolVocab;
	/** Stop condition (default: 1000 steps). */
	stopWhen?: StopCondition<ToolSet>;
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
	messages: UIMessage[];
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

function childReasoningProviderOptions(
	level: SubagentThinkingLevel | undefined,
	fallback: AgentChatSpec<unknown>["reasoningEffort"],
): Record<string, Record<string, JSONValue | undefined>> | undefined {
	const effective = level ?? fallback;
	if (!effective) return undefined;
	if (effective === "off") {
		return { openrouter: { reasoning: { enabled: false } } };
	}
	const effort =
		effective === "minimal"
			? "low"
			: effective === "xhigh"
				? "high"
				: effective;
	return { openrouter: { reasoning: { enabled: true, effort } } };
}

function withAuthoredMergeResult(
	childResult: string,
	finalization: AuthoredSubagentFinalization,
): string {
	if (finalization.status === "conflicted") {
		return `${childResult}\n\n<authored_merge status="conflicted" proposal_revision_id="${finalization.proposalRevisionId}">\n${JSON.stringify({ conflicts: finalization.conflicts })}\n</authored_merge>`;
	}
	return `${childResult}\n\n<authored_merge status="${finalization.status}" revision_id="${finalization.revisionId}"/>`;
}

/** Client-side transport: runs the agent's `streamText` loop in-process and
 * returns a UI-message stream. No HTTP — the model is called directly with the
 * user's key. One transport per chat, bound to that subject's live bridge. */
class DirectStreamTransport<Bridge> implements ChatTransport<UIMessage> {
	constructor(
		private spec: AgentChatSpec<Bridge>,
		private getBridge: () => Bridge | null,
		private threadId: string,
		private subagents: SubagentManager<AuthoredSubagentContext>,
	) {}

	async sendMessages(options: {
		messages: UIMessage[];
		abortSignal: AbortSignal | undefined;
	}): Promise<ReadableStream<UIMessageChunk>> {
		const bridge = this.getBridge();
		if (!bridge) throw new Error("Editor not ready.");
		const model = this.spec.createModel();
		if (!model) {
			throw new Error(
				this.spec.notConfiguredMessage ?? "Agent not configured.",
			);
		}
		this.spec.onTurnStart?.(bridge);
		const turnMessage = [...options.messages]
			.reverse()
			.find((message) => message.role === "user");
		if (!turnMessage) {
			throw new Error("Agent turn has no durable user message.");
		}

		// Tools are built per turn, not once per chat: they need this turn's
		// abort signal, durable user-message identity, and thread id.
		const parentSystemPrompt = this.spec.buildSystem(bridge);
		const parentTools = this.spec.buildTools({
			getBridge: this.getBridge,
			abortSignal: options.abortSignal,
			threadId: this.threadId,
			turnMessageId: turnMessage.id,
		});
		const tools: ToolSet = {
			...parentTools,
			...createSubagentTools(this.subagents, {
				getParentSystemPrompt: () => parentSystemPrompt,
			}),
		};

		const result = streamText({
			model,
			system: parentSystemPrompt,
			messages: await convertToModelMessages(
				options.messages.filter(
					(message) =>
						!message.parts.every((part) => part.type === "data-subagent"),
				),
			),
			tools,
			stopWhen: this.spec.stopWhen ?? stepCountIs(1000),
			abortSignal: options.abortSignal,
			...(this.spec.reasoningEffort
				? {
						providerOptions: {
							openrouter: {
								reasoning: {
									enabled: true,
									effort: this.spec.reasoningEffort,
								},
							},
						},
					}
				: {}),
		});
		return smoothUIMessageStream(result.toUIMessageStream());
	}

	// No server-side stream to resume.
	async reconnectToStream(): Promise<ReadableStream<UIMessageChunk> | null> {
		return null;
	}
}

/** A placeholder chat for components rendered before their thread resolves.
 * `useChat` needs a Chat instance on every render; this one can't send. */
function createIdleChat(): Chat<UIMessage> {
	return new Chat<UIMessage>({
		id: "__idle__",
		transport: {
			sendMessages: async () => {
				throw new Error("Session not ready.");
			},
			reconnectToStream: async () => null,
		},
	});
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
		chat: Chat<UIMessage>;
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
			message: UIMessage;
			messages: UIMessage[];
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
	const idleChat = createIdleChat();
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

	const persist = (session: Session, messages: UIMessage[]): Promise<void> => {
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
				const milestones: UIMessage[] = snapshots.map((snapshot) => ({
					id: `subagent:${snapshot.id}:${snapshot.status}`,
					role: "assistant",
					parts: [{ type: "data-subagent", data: snapshot }],
				}));
				const previous = session.chat.messages;
				const next = [...previous, ...milestones];
				session.chat.messages = next;
				try {
					await persist(session, next);
				} catch (error) {
					session.chat.messages = previous;
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
				(pending.isError
					? (session.chat.error?.message ?? "Agent error")
					: null),
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
		messages: UIMessage[],
		baseline: PersistedMessage[],
	): Session => {
		const threadId = thread.id;
		let session: Session;
		const authoredSubagentContexts = new Set<AuthoredSubagentContext>();
		let subagentCheckpointTail = Promise.resolve();
		const checkpointForSubagent = (): Promise<void> => {
			if (!spec.checkpointAuthoredState) return Promise.resolve();
			const checkpoint = subagentCheckpointTail
				.catch(() => undefined)
				.then(async () => {
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
			(result) =>
				applyAuthoredProjection(
					{ subjectKey, scopeKey, threadId },
					result,
					"subagent",
				),
			checkpointForSubagent,
		);
		const runner = createAiSdkSubagentRunner({
			createModel: (modelId) => {
				const model = spec.createModel(modelId);
				if (!model) {
					throw new Error(spec.notConfiguredMessage ?? "Agent not configured.");
				}
				return model;
			},
			stopWhen: spec.stopWhen,
			providerOptions: ({ thinkingLevel }) =>
				childReasoningProviderOptions(thinkingLevel, spec.reasoningEffort),
		});
		const subagentManager = new SubagentManager<AuthoredSubagentContext>({
			runner,
			agentLoader: new AgentLoader({ definitions: spec.subagentDefinitions }),
			environment:
				"Application: Luma desktop\nWorkspace: isolated authored document files",
			prepareSpawn: async ({ id, parentSubagentId }) => {
				await checkpointForSubagent();
				const workspace = await supervisor.prepare(id, parentSubagentId);
				const context: AuthoredSubagentContext = { workspace };
				authoredSubagentContexts.add(context);
				return {
					tools: workspace.tools,
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
			},
		});
		const chat = new Chat<UIMessage>({
			id: threadId,
			messages,
			transport: new DirectStreamTransport(
				spec,
				() => getBridgeForScope(scopeKey),
				threadId,
				subagentManager,
			),
			// Fires on success, error, and abort alike. The callback captures live
			// state synchronously, then starts the serialized durability protocol;
			// send() and thread switching both await that tail strictly.
			onFinish: ({ message, isAbort, isError }) => {
				if (chats.get(threadId) !== session) return;
				const bridge = getBridgeForScope(scopeKey);
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
									subjectKey,
									threadId,
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
					messages: [...chat.messages],
					isAbort,
					isError,
					capturedState,
					captureError,
					prepared: null,
					result: null,
				};
				void finalizePending(session).catch((error) =>
					reportError(scopeKey, error),
				);
			},
		});
		session = {
			subjectKey,
			scopeKey,
			thread,
			threadId,
			chat,
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
					previous.chat.stop();
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
					previous.unsubscribeSubagents();
					// This is the strict boundary: activation cannot overtake an
					// unpersisted tail from the conversation being left.
					await persist(previous, previous.chat.messages);
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
		const userMessage: UIMessage = {
			id: crypto.randomUUID(),
			role: "user",
			parts: [{ type: "text", text: prompt }],
		};
		const withUser = [...session.chat.messages, userMessage];
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
			await session.chat.sendMessage(userMessage);
			// onFinish installs exactly one finalization attempt. Await that same
			// attempt here so its failure is visible; retries happen only at the
			// next explicit send/switch/restore boundary.
			await session.finalizing;
			await persist(session, session.chat.messages);
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

		const { messages, status, error, stop } = useChat({
			chat: session?.chat ?? idleChat,
		});

		const doSend = useCallback(
			async (text: string) => {
				if (!subjectKey) return;
				await send(subjectKey, text, initRef.current);
			},
			[subjectKey],
		);

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
			streaming: status === "streaming" || status === "submitted",
			error: scopeState?.error ?? error?.message ?? null,
			ready: session !== null,
			switching: scopeState?.switching ?? false,
			restoring: scopeState?.restoring ?? false,
			threads: scopeState?.threads ?? [],
			subagents: session?.subagents ?? [],
			send: doSend,
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
