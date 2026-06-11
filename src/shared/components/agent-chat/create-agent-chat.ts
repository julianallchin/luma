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
import type { ToolVocab } from "./parts";

/**
 * The base agent-chat encapsulation, built on the AI SDK's `useChat` / `Chat`.
 *
 * Each concrete agent (graph editor, track editor, …) supplies an `AgentChatSpec`
 * — model + tools + system + display vocab — and gets back a fully-wired chat.
 * State, streaming, and `UIMessage` accumulation are owned by the SDK (`Chat` +
 * `useChat`); we only provide a client-side `ChatTransport` that runs `streamText`
 * in-process (no server) with the live tools/system.
 *
 * `Bridge` is the agent's live handle on whatever it reads/mutates (the editor,
 * the timeline, …). Tools and the transport resolve it lazily (`getBridge(key)`),
 * so a long-lived session always acts on the current editor.
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
	/** Called once when a turn begins, before streaming. Snapshot the world into
	 * a working copy the tools mutate (so edits don't race UI state). */
	onTurnStart?: (bridge: Bridge) => void;
	/** Called once a turn finishes streaming, with the final assistant message.
	 * Snapshot side state (e.g. the graph) keyed to the message id. */
	onTurnFinish?: (key: string, message: UIMessage, bridge: Bridge) => void;
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
		send: (text: string) => void;
		stop: () => void;
		reset: () => void;
	};
	vocab: ToolVocab;
};

const UI_THROTTLE_MS = 50;
const NONE_KEY = "__none__";

/** Client-side transport: runs the agent's `streamText` loop in-process and
 * returns a UI-message stream. No HTTP — the model is called directly with the
 * user's key. One transport per session, bound to that session's live bridge. */
class DirectStreamTransport<Bridge> implements ChatTransport<UIMessage> {
	private tools: ToolSet;
	constructor(
		private spec: AgentChatSpec<Bridge>,
		private getBridge: () => Bridge | null,
	) {
		this.tools = spec.buildTools(getBridge);
	}

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

		const result = streamText({
			model,
			system: this.spec.buildSystem(bridge),
			messages: await convertToModelMessages(options.messages),
			tools: this.tools,
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
		return result.toUIMessageStream();
	}

	// No server-side stream to resume.
	async reconnectToStream(): Promise<ReadableStream<UIMessageChunk> | null> {
		return null;
	}
}

export function createAgentChat<Bridge>(
	spec: AgentChatSpec<Bridge>,
): AgentChat<Bridge> {
	const bridges = new Map<string, Bridge>();
	const chats = new Map<string, Chat<UIMessage>>();

	const getBridge = (key: string): Bridge | null => bridges.get(key) ?? null;
	const registerBridge = (key: string, bridge: Bridge) => {
		bridges.set(key, bridge);
	};

	const getChat = (key: string): Chat<UIMessage> => {
		const existing = chats.get(key);
		if (existing) return existing;
		const chat = new Chat<UIMessage>({
			id: key,
			transport: new DirectStreamTransport(spec, () => getBridge(key)),
			onFinish: ({ message }) => {
				const bridge = getBridge(key);
				if (bridge) spec.onTurnFinish?.(key, message, bridge);
			},
		});
		chats.set(key, chat);
		return chat;
	};

	const useSession: AgentChat<Bridge>["useSession"] = (key) => {
		const chat = getChat(key ?? NONE_KEY);
		const { messages, status, error, sendMessage, stop, setMessages } = useChat(
			{
				chat,
				experimental_throttle: UI_THROTTLE_MS,
			},
		);
		return {
			messages,
			streaming: status === "streaming" || status === "submitted",
			error: error?.message ?? null,
			send: (text) => {
				if (key) void sendMessage({ text });
			},
			stop,
			reset: () => setMessages([]),
		};
	};

	return { registerBridge, getBridge, useSession, vocab: spec.vocab };
}
