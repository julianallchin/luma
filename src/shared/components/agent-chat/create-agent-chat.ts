import { Chat, useChat } from "@ai-sdk/react";
import {
	type ChatTransport,
	convertToModelMessages,
	type LanguageModel,
	type StopCondition,
	stepCountIs,
	streamText,
	type ToolSet,
	type UIMessage,
	type UIMessageChunk,
} from "ai";
import { useCallback, useEffect, useRef, useState } from "react";
import {
	type AgentKind,
	loadThreadMessages,
	type PersistedMessage,
	resetThread,
	resolveThread,
	type SubjectKind,
	syncThreadMessages,
	type ThreadInit,
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

export type AgentChatSpec<Bridge> = {
	/** Persisted on the thread; scopes thread lookup. */
	agentKind: AgentKind;
	subjectKind: SubjectKind;
	/** Build the language model, or return null if the agent isn't configured
	 * yet (e.g. missing API key) — surfaced to the user as an error. */
	createModel: () => LanguageModel | null;
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
	/** Called once when a turn begins, before streaming. Snapshot the world into
	 * a working copy the tools mutate (so edits don't race UI state). */
	onTurnStart?: (bridge: Bridge) => void;
	/** Called once a turn finishes streaming, with the final assistant message.
	 * Snapshot side state (e.g. the graph) keyed to the message id. */
	onTurnFinish?: (event: TurnFinishedEvent<Bridge>) => void;
	/** Called after the thread's messages have been cleared, so the agent can
	 * drop any ephemeral state it keeps beside the conversation. */
	onReset?: (subjectKey: string) => void;
};

export type AgentSession = {
	/** Null until the durable thread has been resolved. */
	threadId: string | null;
	messages: UIMessage[];
	streaming: boolean;
	error: string | null;
	/** True once the thread is resolved and its history hydrated. */
	ready: boolean;
	send: (text: string) => Promise<void>;
	stop: () => void;
	reset: () => Promise<void>;
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
	/** Send outside React (background batches). Resolves when the turn ends. */
	send: (subjectKey: string, text: string, init?: ThreadInit) => Promise<void>;
	/** Subscribe to a session and get send/stop/reset bound to it. */
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
	const venue = Object.hasOwn(init ?? {}, "venueId")
		? (init?.venueId ?? null)
		: "*";
	const score = Object.hasOwn(init ?? {}, "scoreId")
		? (init?.scoreId ?? null)
		: "*";
	return JSON.stringify([principal, subjectKey, venue, score]);
}

/** Client-side transport: runs the agent's `streamText` loop in-process and
 * returns a UI-message stream. No HTTP — the model is called directly with the
 * user's key. One transport per chat, bound to that subject's live bridge. */
class DirectStreamTransport<Bridge> implements ChatTransport<UIMessage> {
	constructor(
		private spec: AgentChatSpec<Bridge>,
		private getBridge: () => Bridge | null,
		private threadId: string,
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

		// Tools are built per turn, not once per chat: they need this turn's
		// abort signal (so stopping the model stops the tool) and the thread id.
		const tools = this.spec.buildTools({
			getBridge: this.getBridge,
			abortSignal: options.abortSignal,
			threadId: this.threadId,
		});

		const result = streamText({
			model,
			system: this.spec.buildSystem(bridge),
			messages: await convertToModelMessages(options.messages),
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
		threadId: string;
		/** Bumped on reset; suffixes the Chat id so `useChat` re-subscribes. */
		generation: number;
		chat: Chat<UIMessage>;
		baseline: PersistedMessage[];
		/** Serializes writes so a turn's persist can't overtake the previous. */
		persisting: Promise<void>;
		/** Every send through this session, including its terminal persistence. */
		activeTurns: Set<Promise<void>>;
		/** Set while reset drains active turns and clears the durable thread. */
		resetting: Promise<void> | null;
	};

	const bridges = new Map<
		string,
		Array<{ bridge: Bridge; registration: symbol }>
	>();
	/** Live chats, keyed by durable thread id. */
	const chats = new Map<string, Session>();
	const threadByScope = new Map<string, string>();
	const resolving = new Map<string, Promise<Session>>();
	const watchers = new Map<string, Set<() => void>>();
	const finishedListeners = new Set<
		(event: SessionFinishedEvent<Bridge>) => void
	>();
	const idleChat = createIdleChat();

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
		const scopeKey = threadScopeKey(subjectKey, init);
		const registration = Symbol(scopeKey);
		bridges.set(scopeKey, [
			...(bridges.get(scopeKey) ?? []),
			{ bridge, registration },
		]);
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

	const currentSession = (
		subjectKey: string | null,
		init?: ThreadInit,
	): Session | null => {
		if (!subjectKey) return null;
		const threadId = threadByScope.get(threadScopeKey(subjectKey, init));
		return threadId ? (chats.get(threadId) ?? null) : null;
	};

	const notify = (scopeKey: string) => {
		for (const w of watchers.get(scopeKey) ?? []) w();
	};

	const persist = (session: Session, messages: UIMessage[]): Promise<void> => {
		const next = session.persisting
			.then(async () => {
				session.baseline = await syncThreadMessages(
					session.threadId,
					session.baseline,
					messages,
				);
			})
			.catch((err) => {
				console.error("[agent-chat] failed to persist thread messages:", err);
			});
		session.persisting = next;
		return next;
	};

	const makeSession = (
		subjectKey: string,
		scopeKey: string,
		threadId: string,
		generation: number,
		messages: UIMessage[],
		baseline: PersistedMessage[],
	): Session => {
		const chat = new Chat<UIMessage>({
			id: generation === 0 ? threadId : `${threadId}#${generation}`,
			messages,
			transport: new DirectStreamTransport(
				spec,
				() => getBridgeForScope(scopeKey),
				threadId,
			),
			// Fires on success, error, and abort alike — so the transcript is
			// saved even when a turn blew up halfway.
			onFinish: ({ message, isAbort, isError }) => {
				// A reset replaces this Session. Any unexpectedly late callback from
				// the retired Chat must not repopulate the cleared thread or fire side
				// effects against the fresh generation.
				if (chats.get(threadId) !== session) return;
				void persist(session, chat.messages);
				const bridge = getBridgeForScope(scopeKey);
				if (bridge) {
					spec.onTurnFinish?.({ subjectKey, threadId, message, bridge });
				}
				const event: SessionFinishedEvent<Bridge> = {
					subjectKey,
					threadId,
					bridge,
					error: isError ? (chat.error?.message ?? "Agent error") : null,
					aborted: isAbort,
				};
				for (const listener of finishedListeners) {
					try {
						listener(event);
					} catch (err) {
						console.error("[agent-chat] session-finished listener threw:", err);
					}
				}
			},
		});
		const session: Session = {
			subjectKey,
			scopeKey,
			threadId,
			generation,
			chat,
			baseline,
			persisting: Promise.resolve(),
			activeTurns: new Set(),
			resetting: null,
		};
		return session;
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

		const promise = (async () => {
			const thread = await resolveThread(
				spec.agentKind,
				spec.subjectKind,
				subjectKey,
				init,
			);
			const { messages, baseline } = await loadThreadMessages(thread.id);
			const session = makeSession(
				subjectKey,
				scopeKey,
				thread.id,
				0,
				messages,
				baseline,
			);
			threadByScope.set(scopeKey, thread.id);
			chats.set(thread.id, session);
			notify(scopeKey);
			return session;
		})();
		resolving.set(scopeKey, promise);
		void promise.finally(() => {
			if (resolving.get(scopeKey) === promise) resolving.delete(scopeKey);
		});
		return promise;
	};

	const send = async (
		subjectKey: string,
		text: string,
		init?: ThreadInit,
	): Promise<void> => {
		const prompt = text.trim();
		if (!prompt) return;
		let session = await ensureSession(subjectKey, init);
		if (session.resetting) {
			await session.resetting;
			session = await ensureSession(subjectKey, init);
		}
		const userMessage: UIMessage = {
			id: crypto.randomUUID(),
			role: "user",
			parts: [{ type: "text", text: prompt }],
		};
		// Persist the prompt before the turn runs: an interrupted or crashed turn
		// must not lose what the user asked for.
		const withUser = [...session.chat.messages, userMessage];
		const turn = session.chat.sendMessage(userMessage);
		void persist(session, withUser);
		const completion = (async () => {
			await turn;
			// onFinish persists fire-and-forget (fine for the UI); a non-React
			// driver needs send() to mean "turn persisted". persist() chains on
			// session.persisting and no-ops when already in sync.
			await persist(session, session.chat.messages);
		})();
		session.activeTurns.add(completion);
		try {
			await completion;
		} finally {
			session.activeTurns.delete(completion);
		}
	};

	const reset = async (
		subjectKey: string,
		init?: ThreadInit,
	): Promise<void> => {
		const scopeKey = threadScopeKey(subjectKey, init);
		const session = currentSession(subjectKey, init);
		if (!session) return;
		if (session.resetting) return session.resetting;

		const operation = (async () => {
			// Abort first, then wait for every turn's onFinish + persistence path.
			// Clearing before that drain would let the retired Chat append an old
			// assistant tail back into the newly empty durable thread.
			void session.chat.stop();
			await Promise.allSettled([...session.activeTurns]);
			await session.persisting;

			// One command clears messages and the thread-owned Python workspace.
			await resetThread(session.threadId);
			const fresh = makeSession(
				subjectKey,
				session.scopeKey,
				session.threadId,
				session.generation + 1,
				[],
				[],
			);
			chats.set(session.threadId, fresh);
			spec.onReset?.(subjectKey);
			notify(scopeKey);
		})();
		session.resetting = operation;
		try {
			await operation;
		} finally {
			if (session.resetting === operation) session.resetting = null;
		}
	};

	const useSession: AgentChat<Bridge>["useSession"] = (subjectKey, init) => {
		const scopeKey = subjectKey ? threadScopeKey(subjectKey, init) : null;
		const [session, setSession] = useState<Session | null>(() =>
			currentSession(subjectKey, init),
		);
		const [resolveError, setResolveError] = useState<string | null>(null);
		const initRef = useRef(init);
		initRef.current = init;

		useEffect(() => {
			if (!subjectKey) {
				setSession(null);
				return;
			}
			let live = true;
			const sync = () => {
				if (live) setSession(currentSession(subjectKey, initRef.current));
			};
			if (!scopeKey) return;
			const set = watchers.get(scopeKey) ?? new Set<() => void>();
			set.add(sync);
			watchers.set(scopeKey, set);
			sync();
			ensureSession(subjectKey, initRef.current)
				.then(() => {
					if (live) setResolveError(null);
				})
				.catch((err: unknown) => {
					if (live) {
						setResolveError(err instanceof Error ? err.message : String(err));
					}
				});
			return () => {
				live = false;
				set.delete(sync);
			};
		}, [subjectKey, scopeKey]);

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

		const doReset = useCallback(async () => {
			if (!subjectKey) return;
			await reset(subjectKey, initRef.current);
		}, [subjectKey]);

		return {
			threadId: session?.threadId ?? null,
			messages,
			streaming: status === "streaming" || status === "submitted",
			error: resolveError ?? error?.message ?? null,
			ready: session !== null,
			send: doSend,
			stop,
			reset: doReset,
		};
	};

	return {
		registerBridge,
		getBridge,
		resolveThreadFor: async (subjectKey, init) =>
			(await ensureSession(subjectKey, init)).threadId,
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
