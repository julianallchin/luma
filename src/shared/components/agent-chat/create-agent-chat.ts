import {
	convertToModelMessages,
	type LanguageModel,
	readUIMessageStream,
	type StopCondition,
	stepCountIs,
	streamText,
	type ToolSet,
	type UIMessage,
} from "ai";
import { useEffect } from "react";
import { create } from "zustand";
import type { ToolVocab } from "./parts";

/**
 * The base agent-chat encapsulation.
 *
 * Each concrete agent (graph editor, track editor, …) supplies an `AgentChatSpec`
 * — model + tools + system + display vocab — and gets back a fully-wired chat:
 * a zustand store keyed by session, a `useSession` hook, and a live-bridge
 * registry. Streaming is 100% SDK-native (`streamText` → `readUIMessageStream`),
 * so messages are real `UIMessage`s and there's no hand-rolled reducer.
 *
 * `Bridge` is the agent's live handle on whatever it reads/mutates (the editor,
 * the timeline, …). Tools close over a *resolver* (`getBridge(key)`) rather than
 * a captured bridge, so a long-lived session always acts on the current editor.
 */
export type AgentChatSpec<Bridge> = {
	/** Build the language model, or return null if the agent isn't configured
	 * yet (e.g. missing API key) — surfaced to the user as an error. */
	createModel: () => LanguageModel | null;
	/** Message shown when createModel returns null. */
	notConfiguredMessage?: string;
	/** Build the tool set, bound to a resolver that always yields the live bridge. */
	buildTools: (getBridge: () => Bridge | null) => ToolSet;
	/** System prompt for a turn; may read the live bridge for context. */
	buildSystem: (bridge: Bridge) => string;
	/** Tool-run display vocabulary for the shared renderer. */
	vocab: ToolVocab;
	/** Stop condition (default: 1000 steps). */
	stopWhen?: StopCondition<ToolSet>;
	/** Reasoning effort hint for providers that support it. */
	reasoningEffort?: "low" | "medium" | "high";
	/** Called once when a turn begins, before streaming. Use it to snapshot the
	 * current world into a working copy the tools mutate (so edits within a turn
	 * don't race async UI state, and manual edits between turns are picked up). */
	onTurnStart?: (bridge: Bridge) => void;
	/** Called once a turn finishes streaming, with the final assistant message.
	 * Use it to snapshot side state (e.g. the graph) keyed to the message id. */
	onTurnFinish?: (key: string, message: UIMessage, bridge: Bridge) => void;
};

type Session = {
	key: string;
	messages: UIMessage[];
	streaming: boolean;
	error: string | null;
};

type Store = {
	sessions: Record<string, Session>;
	ensure: (key: string) => Session;
};

export type AgentChat<Bridge> = {
	/** Register/refresh the live bridge for a session (call from an effect). */
	registerBridge: (key: string, bridge: Bridge) => void;
	getBridge: (key: string) => Bridge | null;
	/** Subscribe to a session and get send/stop/reset bound to it. */
	useSession: (key: string | null) => {
		messages: UIMessage[];
		streaming: boolean;
		error: string | null;
		send: (text: string) => Promise<void>;
		stop: () => void;
		reset: () => void;
	};
	vocab: ToolVocab;
};

const EMPTY: UIMessage[] = [];

export function createAgentChat<Bridge>(
	spec: AgentChatSpec<Bridge>,
): AgentChat<Bridge> {
	const bridges = new Map<string, Bridge>();
	const aborters = new Map<string, AbortController>();

	const useStore = create<Store>((set, get) => ({
		sessions: {},
		ensure: (key) => {
			const existing = get().sessions[key];
			if (existing) return existing;
			const fresh: Session = {
				key,
				messages: [],
				streaming: false,
				error: null,
			};
			set((s) => ({ sessions: { ...s.sessions, [key]: fresh } }));
			return fresh;
		},
	}));

	const patch = (key: string, next: Partial<Session>) => {
		useStore.setState((s) => {
			const existing = s.sessions[key];
			if (!existing) return {} as Partial<Store>;
			return { sessions: { ...s.sessions, [key]: { ...existing, ...next } } };
		});
	};

	const registerBridge = (key: string, bridge: Bridge) => {
		bridges.set(key, bridge);
	};
	const getBridge = (key: string): Bridge | null => bridges.get(key) ?? null;

	// Tools are built once, closing over a live resolver. The resolver reads the
	// bridge for whichever session is currently streaming.
	let streamingKey: string | null = null;
	const tools = spec.buildTools(() =>
		streamingKey ? getBridge(streamingKey) : null,
	);

	const send = async (key: string, text: string) => {
		const trimmed = text.trim();
		if (!trimmed) return;
		const bridge = getBridge(key);
		if (!bridge) {
			patch(key, { error: "Editor not ready." });
			return;
		}
		const model = spec.createModel();
		if (!model) {
			patch(key, {
				error: spec.notConfiguredMessage ?? "Agent is not configured.",
			});
			return;
		}

		useStore.getState().ensure(key);
		const prior = useStore.getState().sessions[key]?.messages ?? [];
		const userMessage: UIMessage = {
			id: crypto.randomUUID(),
			role: "user",
			parts: [{ type: "text", text: trimmed }],
		};
		const history = [...prior, userMessage];
		patch(key, { messages: history, streaming: true, error: null });

		const aborter = new AbortController();
		aborters.set(key, aborter);
		streamingKey = key;
		spec.onTurnStart?.(bridge);

		// Per-turn reasoning timing, attached onto reasoning parts so the renderer
		// can show "Thought for Ns" without the SDK carrying timestamps.
		const reasoningTiming = new Map<
			number,
			{ startedAt: number; lastDeltaAt: number; len: number }
		>();
		const stampTiming = (msg: UIMessage): UIMessage => {
			const now = Date.now();
			let i = -1;
			for (const part of msg.parts) {
				i += 1;
				if (part.type !== "reasoning") continue;
				const prev = reasoningTiming.get(i);
				const len = part.text.length;
				if (!prev) {
					reasoningTiming.set(i, { startedAt: now, lastDeltaAt: now, len });
				} else if (len > prev.len) {
					prev.lastDeltaAt = now;
					prev.len = len;
				}
				const t = reasoningTiming.get(i);
				if (t) {
					(part as { startedAt?: number; lastDeltaAt?: number }).startedAt =
						t.startedAt;
					(part as { startedAt?: number; lastDeltaAt?: number }).lastDeltaAt =
						t.lastDeltaAt;
				}
			}
			return msg;
		};

		let finalAssistant: UIMessage | null = null;
		try {
			const result = streamText({
				model,
				system: spec.buildSystem(bridge),
				messages: await convertToModelMessages(history),
				tools,
				stopWhen: spec.stopWhen ?? stepCountIs(1000),
				abortSignal: aborter.signal,
				...(spec.reasoningEffort
					? {
							providerOptions: {
								openrouter: {
									reasoning: { enabled: true, effort: spec.reasoningEffort },
								},
							},
						}
					: {}),
			});

			for await (const snapshot of readUIMessageStream({
				stream: result.toUIMessageStream(),
			})) {
				if (aborter.signal.aborted) break;
				finalAssistant = stampTiming(snapshot);
				patch(key, { messages: [...history, finalAssistant] });
			}

			if (finalAssistant) {
				spec.onTurnFinish?.(key, finalAssistant, bridge);
			}
		} catch (err) {
			if (!aborter.signal.aborted) {
				patch(key, {
					error: err instanceof Error ? err.message : String(err),
				});
			}
		} finally {
			patch(key, { streaming: false });
			if (aborters.get(key) === aborter) aborters.delete(key);
			if (streamingKey === key) streamingKey = null;
		}
	};

	const stop = (key: string) => aborters.get(key)?.abort();

	const reset = (key: string) => {
		aborters.get(key)?.abort();
		patch(key, { messages: [], error: null });
	};

	const useSession: AgentChat<Bridge>["useSession"] = (key) => {
		const session = useStore((s) => (key ? s.sessions[key] : undefined));
		// Ensure the session exists on first subscribe.
		useEffect(() => {
			if (key) useStore.getState().ensure(key);
		}, [key]);
		return {
			messages: session?.messages ?? EMPTY,
			streaming: session?.streaming ?? false,
			error: session?.error ?? null,
			send: async (text) => {
				if (key) await send(key, text);
			},
			stop: () => {
				if (key) stop(key);
			},
			reset: () => {
				if (key) reset(key);
			},
		};
	};

	return { registerBridge, getBridge, useSession, vocab: spec.vocab };
}
