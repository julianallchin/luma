import { createOpenRouter } from "@openrouter/ai-sdk-provider";
import { invoke } from "@tauri-apps/api/core";
import type { ModelMessage } from "ai";
import { stepCountIs, streamText } from "ai";
import { create } from "zustand";
import type {
	BeatGrid,
	PatternArgDef,
	PatternSummary,
	ScoreSummary,
	TrackScore,
} from "@/bindings/schema";
import type {
	TimelineAnnotation,
	TrackWaveform,
} from "../stores/use-track-editor-store";
import { useTrackEditorStore } from "../stores/use-track-editor-store";
import {
	type BarClassificationsPayload,
	buildSystemPrompt,
	type DrumOnsets,
	formatBarTags,
} from "./build-context";
import { getOpenRouterKey, OPENROUTER_MODEL } from "./openrouter-key";
import { formatSummary } from "./score-view";
import { buildAgentTools } from "./tools";

const PATTERN_COLORS = [
	"#8b5cf6",
	"#ec4899",
	"#f59e0b",
	"#10b981",
	"#3b82f6",
	"#ef4444",
	"#06b6d4",
	"#f97316",
];

function getPatternColor(patternId: string): string {
	let hash = 0;
	for (let i = 0; i < patternId.length; i++) {
		hash = (hash * 31 + patternId.charCodeAt(i)) | 0;
	}
	return PATTERN_COLORS[Math.abs(hash) % PATTERN_COLORS.length];
}

export type ToolPart = {
	id: string;
	name: string;
	input: unknown;
	output?: unknown;
	error?: string;
	state: "input-streaming" | "executing" | "done" | "error";
};

export type ChatTextPart = { kind: "text"; id: string; text: string };
export type ChatReasoningPart = {
	kind: "reasoning";
	id: string;
	text: string;
	startedAt: number;
	lastDeltaAt: number;
};
export type ChatToolPart = { kind: "tool"; tool: ToolPart };
export type ChatPart = ChatTextPart | ChatReasoningPart | ChatToolPart;

export type ChatMessage =
	| { id: string; role: "user"; text: string }
	| { id: string; role: "assistant"; parts: ChatPart[] };

/** Per-track snapshot of everything the agent / its tools need. Owned by the
 * session, not the editor store, so the agent keeps working after the user
 * navigates back to the track list. */
export type SessionContext = {
	venueId: string;
	scoreId: string;
	readOnly: boolean;
	trackName: string;
	durationSeconds: number;
	beatGrid: BeatGrid | null;
	annotations: TimelineAnnotation[];
	patterns: PatternSummary[];
	patternArgs: Record<string, PatternArgDef[]>;
	venueName: string | null;
	barClassifications: BarClassificationsPayload | null;
	drumOnsets: DrumOnsets | null;
	tagThresholds: Record<string, number>;
};

export type ChatSession = {
	trackId: string;
	context: SessionContext | null;
	messages: ChatMessage[];
	streaming: boolean;
	error: string | null;
	bootstrapping: boolean;
};

export type SendArgs = {
	prompt: string;
};

type SessionsState = {
	sessions: Record<string, ChatSession>;
	getSession: (trackId: string) => ChatSession | undefined;
	ensureSession: (trackId: string) => ChatSession;
	updateContext: (trackId: string, partial: Partial<SessionContext>) => void;
	bootstrap: (args: BootstrapArgs) => Promise<BootstrapResult>;
	send: (trackId: string, args: SendArgs) => Promise<void>;
	abort: (trackId: string) => void;
	reset: (trackId: string) => void;
	/** Replace a session's annotations (called from inside tool handlers). */
	setAnnotations: (trackId: string, annotations: TimelineAnnotation[]) => void;
	/** Listener fired when a background send completes (success or error). The
	 * subscriber decides whether to surface a toast based on the user's
	 * current view. */
	onSessionFinished: (
		listener: (e: SessionFinishedEvent) => void,
	) => () => void;
};

export type SessionFinishedEvent = {
	trackId: string;
	trackName: string;
	error: string | null;
};

export type BootstrapArgs = {
	trackId: string;
	venueId: string;
	venueName: string | null;
	userId: string;
	trackName: string;
	/** Existing scoreId. If omitted, the most recent own score is used; if
	 * none exists, a fresh score is created. */
	scoreId?: string;
	/** Pass true to allow loading a score owned by someone else as read-only.
	 * Defaults to false (auto-light needs to write). */
	allowReadOnly?: boolean;
};

export type BootstrapResult =
	| { ok: true; session: ChatSession; createdScore: boolean }
	| { ok: false; error: string };

const finishedListeners = new Set<(e: SessionFinishedEvent) => void>();

// Module-level: AbortControllers don't belong in tracked state (no need to
// re-render on identity change) and would force consumers to wrap them.
const abortControllers = new Map<string, AbortController>();

function emptySession(trackId: string): ChatSession {
	return {
		trackId,
		context: null,
		messages: [],
		streaming: false,
		error: null,
		bootstrapping: false,
	};
}

export const useChatSessionsStore = create<SessionsState>((set, get) => ({
	sessions: {},

	getSession: (trackId) => get().sessions[trackId],

	ensureSession: (trackId) => {
		const existing = get().sessions[trackId];
		if (existing) return existing;
		const fresh = emptySession(trackId);
		set((state) => ({ sessions: { ...state.sessions, [trackId]: fresh } }));
		return fresh;
	},

	updateContext: (trackId, partial) => {
		set((state) => {
			const existing = state.sessions[trackId] ?? emptySession(trackId);
			const merged: SessionContext = {
				...(existing.context ?? defaultContext()),
				...partial,
			};
			return {
				sessions: {
					...state.sessions,
					[trackId]: { ...existing, context: merged },
				},
			};
		});
	},

	setAnnotations: (trackId, annotations) => {
		set((state) => {
			const existing = state.sessions[trackId];
			if (!existing?.context) return {};
			return {
				sessions: {
					...state.sessions,
					[trackId]: {
						...existing,
						context: { ...existing.context, annotations },
					},
				},
			};
		});
	},

	bootstrap: async (args) => {
		const { trackId, venueId, venueName, userId, trackName, scoreId } = args;

		// Mark bootstrapping in flight (and seed an empty session if needed).
		set((state) => {
			const existing = state.sessions[trackId] ?? emptySession(trackId);
			return {
				sessions: {
					...state.sessions,
					[trackId]: { ...existing, bootstrapping: true, error: null },
				},
			};
		});

		try {
			// Resolve the score to work against.
			let resolvedScoreId = scoreId ?? null;
			let readOnly = false;
			let createdScore = false;

			if (!resolvedScoreId) {
				const scores = await invoke<ScoreSummary[]>("list_scores_for_track", {
					trackId,
					venueId,
				});
				const ownScore = scores.find((s) => s.uid === userId);
				if (ownScore) {
					resolvedScoreId = ownScore.id;
				} else {
					const created = await invoke<{ id: string }>("create_score", {
						trackId,
						venueId,
						uid: userId,
						name: null,
					});
					resolvedScoreId = created.id;
					createdScore = true;
				}
			} else {
				// Verify the existing score's ownership for read-only flag.
				const scores = await invoke<ScoreSummary[]>("list_scores_for_track", {
					trackId,
					venueId,
				});
				const found = scores.find((s) => s.id === resolvedScoreId);
				if (found && found.uid !== userId) {
					if (!args.allowReadOnly) {
						throw new Error("Score is owned by another user (read-only).");
					}
					readOnly = true;
				}
			}

			// Make sure patterns are loaded in the editor store (cached if so).
			// We reuse them directly to avoid duplicating expensive arg fetches.
			if (useTrackEditorStore.getState().patterns.length === 0) {
				await useTrackEditorStore.getState().loadPatterns();
			}
			const editorState = useTrackEditorStore.getState();
			const patterns = editorState.patterns;
			const patternArgs = editorState.patternArgs;

			// Fetch the rest in parallel.
			const [
				beatGrid,
				waveform,
				rawAnnotations,
				barClassifications,
				drumOnsets,
				tagThresholds,
			] = await Promise.all([
				invoke<BeatGrid | null>("get_track_beats", { trackId }).catch(
					() => null,
				),
				invoke<TrackWaveform>("get_track_waveform", { trackId }).catch(
					() => null,
				),
				invoke<TrackScore[]>("list_track_scores", {
					scoreId: resolvedScoreId,
				}).catch(() => [] as TrackScore[]),
				invoke<{
					classifications: BarClassificationsPayload["classifications"];
					tagOrder: string[];
				} | null>("get_track_bar_classifications", { trackId }).catch(
					() => null,
				),
				invoke<DrumOnsets | null>("get_track_drum_onsets", { trackId }).catch(
					() => null,
				),
				invoke<Record<string, number>>("get_classifier_thresholds").catch(
					() => ({}) as Record<string, number>,
				),
			]);

			const annotations: TimelineAnnotation[] = rawAnnotations.map((ann) => {
				const pattern = patterns.find((p) => p.id === ann.patternId);
				return {
					...ann,
					patternName: pattern?.name,
					patternColor: getPatternColor(ann.patternId),
				};
			});

			if (!resolvedScoreId) {
				throw new Error("Failed to resolve a score id.");
			}
			const context: SessionContext = {
				venueId,
				scoreId: resolvedScoreId,
				readOnly,
				trackName,
				durationSeconds: waveform?.durationSeconds ?? 0,
				beatGrid,
				annotations,
				patterns,
				patternArgs,
				venueName,
				barClassifications: barClassifications
					? {
							classifications: barClassifications.classifications,
							tagOrder: barClassifications.tagOrder,
						}
					: null,
				drumOnsets,
				tagThresholds,
			};

			set((state) => {
				const existing = state.sessions[trackId] ?? emptySession(trackId);
				return {
					sessions: {
						...state.sessions,
						[trackId]: {
							...existing,
							context,
							bootstrapping: false,
							error: null,
						},
					},
				};
			});

			const finalSession = get().sessions[trackId] ?? emptySession(trackId);
			return { ok: true as const, session: finalSession, createdScore };
		} catch (err) {
			const message = err instanceof Error ? err.message : String(err);
			set((state) => {
				const existing = state.sessions[trackId] ?? emptySession(trackId);
				return {
					sessions: {
						...state.sessions,
						[trackId]: { ...existing, bootstrapping: false, error: message },
					},
				};
			});
			return { ok: false as const, error: message };
		}
	},

	send: async (trackId, { prompt }) => {
		const apiKey = getOpenRouterKey();
		if (!apiKey) {
			set((state) => {
				const existing = state.sessions[trackId] ?? emptySession(trackId);
				return {
					sessions: {
						...state.sessions,
						[trackId]: {
							...existing,
							error: "OpenRouter API key is not set.",
						},
					},
				};
			});
			return;
		}

		const text = prompt.trim();
		if (!text) return;

		const session = get().sessions[trackId];
		if (!session?.context) {
			set((state) => {
				const existing = state.sessions[trackId] ?? emptySession(trackId);
				return {
					sessions: {
						...state.sessions,
						[trackId]: {
							...existing,
							error: "Session not bootstrapped.",
						},
					},
				};
			});
			return;
		}

		const userId = crypto.randomUUID();
		const assistantId = crypto.randomUUID();

		// Snapshot current messages so we send them with the user turn appended.
		const priorMessages = session.messages;
		const userMessage: ChatMessage = { id: userId, role: "user", text };
		const assistantSeed: ChatMessage = {
			id: assistantId,
			role: "assistant",
			parts: [],
		};

		set((state) => {
			const existing = state.sessions[trackId] ?? emptySession(trackId);
			return {
				sessions: {
					...state.sessions,
					[trackId]: {
						...existing,
						messages: [...existing.messages, userMessage, assistantSeed],
						streaming: true,
						error: null,
					},
				},
			};
		});

		const tools = buildAgentTools({
			getContext: () => {
				const ctx = get().sessions[trackId]?.context;
				if (!ctx) return null;
				return {
					trackId,
					venueId: ctx.venueId,
					scoreId: ctx.scoreId,
					readOnly: ctx.readOnly,
					durationSeconds: ctx.durationSeconds,
					beatGrid: ctx.beatGrid,
					annotations: ctx.annotations,
					patterns: ctx.patterns,
					patternArgs: ctx.patternArgs,
				};
			},
			setAnnotations: (annotations) => {
				get().setAnnotations(trackId, annotations);
				// If the editor is open for this same track, push the new
				// annotations in so the visualizer/timeline reflect agent edits
				// live. Skip the reload — we already have the fresh list.
				const editor = useTrackEditorStore.getState();
				if (editor.trackId === trackId) {
					useTrackEditorStore.setState({ annotations });
				}
			},
		});

		const ctx = session.context;
		const system = `${buildSystemPrompt({
			trackName: ctx.trackName,
			durationSeconds: ctx.durationSeconds,
			beatGrid: ctx.beatGrid,
			patterns: ctx.patterns,
			venueName: ctx.venueName,
			annotationsCount: ctx.annotations.length,
		})}

## Bar-by-bar tags & drum grid
${formatBarTags(ctx.barClassifications, ctx.tagThresholds, ctx.drumOnsets, ctx.beatGrid?.beats ?? null)}

## Current score (summary — call view_score for detail)
${formatSummary(ctx.annotations, ctx.beatGrid, ctx.durationSeconds)}`;

		const modelMessages: ModelMessage[] = toModelMessages(priorMessages);
		modelMessages.push({ role: "user", content: text });

		const abortController = new AbortController();
		abortControllers.set(trackId, abortController);

		try {
			const openrouter = createOpenRouter({
				apiKey,
				appName: "Luma",
				appUrl: "https://luma.show",
			});
			const result = streamText({
				model: openrouter(OPENROUTER_MODEL),
				system,
				messages: modelMessages,
				tools,
				stopWhen: stepCountIs(1000),
				abortSignal: abortController.signal,
				providerOptions: {
					openrouter: {
						reasoning: { enabled: true, effort: "medium" },
					},
				},
			});

			for await (const part of result.fullStream) {
				if (abortController.signal.aborted) break;
				appendStreamPart(set, get, trackId, assistantId, part);
			}

			notifyFinished({
				trackId,
				trackName: ctx.trackName,
				error: null,
			});
		} catch (err: unknown) {
			if (
				err instanceof Error &&
				(err.name === "AbortError" || abortController.signal.aborted)
			) {
				// Aborted intentionally — no toast.
			} else {
				const message = err instanceof Error ? err.message : String(err);
				set((state) => {
					const existing = state.sessions[trackId];
					if (!existing) return {};
					return {
						sessions: {
							...state.sessions,
							[trackId]: { ...existing, error: message },
						},
					};
				});
				notifyFinished({
					trackId,
					trackName: ctx.trackName,
					error: message,
				});
			}
		} finally {
			set((state) => {
				const existing = state.sessions[trackId];
				if (!existing) return {};
				return {
					sessions: {
						...state.sessions,
						[trackId]: { ...existing, streaming: false },
					},
				};
			});
			if (abortControllers.get(trackId) === abortController) {
				abortControllers.delete(trackId);
			}
		}
	},

	abort: (trackId) => {
		abortControllers.get(trackId)?.abort();
	},

	reset: (trackId) => {
		abortControllers.get(trackId)?.abort();
		set((state) => {
			const existing = state.sessions[trackId];
			if (!existing) return {};
			return {
				sessions: {
					...state.sessions,
					[trackId]: { ...existing, messages: [], error: null },
				},
			};
		});
	},

	onSessionFinished: (listener) => {
		finishedListeners.add(listener);
		return () => finishedListeners.delete(listener);
	},
}));

function notifyFinished(e: SessionFinishedEvent) {
	for (const listener of finishedListeners) {
		try {
			listener(e);
		} catch (err) {
			console.error("[chat-sessions] finished listener threw:", err);
		}
	}
}

// Mirror the editor store's view into the matching session whenever its
// inputs change. This keeps the agent's context fresh when the user makes
// manual edits in the timeline (drag, restack, paste, etc.) without each
// editor action having to know about sessions.
useTrackEditorStore.subscribe((state, prev) => {
	const trackId = state.trackId;
	if (!trackId) return;
	const sessions = useChatSessionsStore.getState().sessions;
	const session = sessions[trackId];
	if (!session?.context) return;

	const ctx = session.context;
	const next: Partial<SessionContext> = {};
	let changed = false;

	if (
		state.annotations !== prev.annotations &&
		state.annotations !== ctx.annotations
	) {
		next.annotations = state.annotations;
		changed = true;
	}
	if (state.beatGrid !== prev.beatGrid && state.beatGrid !== ctx.beatGrid) {
		next.beatGrid = state.beatGrid;
		changed = true;
	}
	if (
		state.durationSeconds !== prev.durationSeconds &&
		state.durationSeconds !== ctx.durationSeconds
	) {
		next.durationSeconds = state.durationSeconds;
		changed = true;
	}
	if (state.patterns !== prev.patterns && state.patterns !== ctx.patterns) {
		next.patterns = state.patterns;
		changed = true;
	}
	if (
		state.patternArgs !== prev.patternArgs &&
		state.patternArgs !== ctx.patternArgs
	) {
		next.patternArgs = state.patternArgs;
		changed = true;
	}
	if (
		state.scoreId &&
		state.scoreId !== prev.scoreId &&
		state.scoreId !== ctx.scoreId
	) {
		next.scoreId = state.scoreId;
		changed = true;
	}
	if (state.readOnly !== prev.readOnly && state.readOnly !== ctx.readOnly) {
		next.readOnly = state.readOnly;
		changed = true;
	}
	if (
		state.trackName &&
		state.trackName !== prev.trackName &&
		state.trackName !== ctx.trackName
	) {
		next.trackName = state.trackName;
		changed = true;
	}
	if (changed) {
		useChatSessionsStore.getState().updateContext(trackId, next);
	}
});

function defaultContext(): SessionContext {
	return {
		venueId: "",
		scoreId: "",
		readOnly: false,
		trackName: "",
		durationSeconds: 0,
		beatGrid: null,
		annotations: [],
		patterns: [],
		patternArgs: {},
		venueName: null,
		barClassifications: null,
		drumOnsets: null,
		tagThresholds: {},
	};
}

function toModelMessages(uiMessages: ChatMessage[]): ModelMessage[] {
	const out: ModelMessage[] = [];
	for (const m of uiMessages) {
		if (m.role === "user") {
			out.push({ role: "user", content: m.text });
			continue;
		}
		const text = m.parts
			.filter((p): p is ChatTextPart => p.kind === "text")
			.map((p) => p.text)
			.join("");
		if (text.trim().length > 0) {
			out.push({ role: "assistant", content: text });
		}
	}
	return out;
}

type StreamPart = {
	type: string;
	id?: string;
	text?: string;
	toolName?: string;
	toolCallId?: string;
	input?: unknown;
	output?: unknown;
	error?: unknown;
	delta?: string;
};

const DEBUG_CHAT = false;

type SetState = (
	updater: (state: SessionsState) => Partial<SessionsState>,
) => void;
type GetState = () => SessionsState;

function appendStreamPart(
	set: SetState,
	get: GetState,
	trackId: string,
	assistantId: string,
	part: StreamPart,
) {
	if (
		DEBUG_CHAT &&
		(part.type === "tool-call" ||
			part.type === "tool-input-start" ||
			part.type === "tool-result" ||
			part.type === "tool-error")
	) {
		console.log("[chat]", part.type, {
			trackId,
			tool: part.toolName,
			id: part.toolCallId?.slice(-8),
		});
	}

	const mutateMessages = (mutator: (parts: ChatPart[]) => ChatPart[]) => {
		set((state) => {
			const existing = state.sessions[trackId];
			if (!existing) return {};
			const messages = existing.messages.map((m) => {
				if (m.role !== "assistant" || m.id !== assistantId) return m;
				return { ...m, parts: mutator(m.parts) };
			});
			return {
				sessions: {
					...state.sessions,
					[trackId]: { ...existing, messages },
				},
			};
		});
	};

	if (part.type === "text-delta") {
		const id = part.id ?? "default";
		const delta = part.text ?? "";
		mutateMessages((parts) => appendTextDelta(parts, id, delta));
		return;
	}

	if (part.type === "reasoning-delta") {
		const id = part.id ?? "default";
		const delta = part.text ?? "";
		mutateMessages((parts) => appendReasoningDelta(parts, id, delta));
		return;
	}

	if (part.type === "tool-input-start") {
		const id = part.toolCallId ?? part.id ?? "";
		const name = part.toolName ?? "tool";
		mutateMessages((parts) =>
			upsertToolPart(parts, {
				id,
				name,
				input: undefined,
				state: "input-streaming",
			}),
		);
		return;
	}

	if (part.type === "tool-call") {
		const id = part.toolCallId ?? "";
		const name = part.toolName ?? "tool";
		mutateMessages((parts) =>
			upsertToolPart(parts, {
				id,
				name,
				input: part.input,
				state: "executing",
			}),
		);
		return;
	}

	if (part.type === "tool-result") {
		const id = part.toolCallId ?? "";
		const name = part.toolName ?? "tool";
		mutateMessages((parts) =>
			upsertToolPart(parts, {
				id,
				name,
				input: part.input,
				output: part.output,
				state: "done",
			}),
		);
		return;
	}

	if (part.type === "tool-error") {
		const id = part.toolCallId ?? "";
		const name = part.toolName ?? "tool";
		mutateMessages((parts) =>
			upsertToolPart(parts, {
				id,
				name,
				input: part.input,
				error:
					part.error instanceof Error
						? part.error.message
						: String(part.error ?? "tool error"),
				state: "error",
			}),
		);
	}
	void get;
}

function appendTextDelta(
	parts: ChatPart[],
	id: string,
	delta: string,
): ChatPart[] {
	const last = parts[parts.length - 1];
	if (last && last.kind === "text" && last.id === id) {
		return [...parts.slice(0, -1), { ...last, text: last.text + delta }];
	}
	return [...parts, { kind: "text", id, text: delta }];
}

function appendReasoningDelta(
	parts: ChatPart[],
	id: string,
	delta: string,
): ChatPart[] {
	const now = Date.now();
	const last = parts[parts.length - 1];
	if (last && last.kind === "reasoning" && last.id === id) {
		return [
			...parts.slice(0, -1),
			{ ...last, text: last.text + delta, lastDeltaAt: now },
		];
	}
	return [
		...parts,
		{ kind: "reasoning", id, text: delta, startedAt: now, lastDeltaAt: now },
	];
}

function upsertToolPart(
	parts: ChatPart[],
	tool: Pick<ToolPart, "id" | "name" | "input" | "output" | "error" | "state">,
): ChatPart[] {
	let idx = -1;
	for (let i = parts.length - 1; i >= 0; i--) {
		const p = parts[i];
		if (
			p.kind === "tool" &&
			stripIdSuffix(p.tool.id) === tool.id &&
			p.tool.state !== "done" &&
			p.tool.state !== "error"
		) {
			idx = i;
			break;
		}
	}
	if (idx === -1) {
		const dupCount = parts.filter(
			(p) => p.kind === "tool" && stripIdSuffix(p.tool.id) === tool.id,
		).length;
		const uniqueId = dupCount === 0 ? tool.id : `${tool.id}#${dupCount}`;
		return [...parts, { kind: "tool", tool: { ...tool, id: uniqueId } }];
	}
	const existing = parts[idx] as ChatToolPart;
	const merged: ChatToolPart = {
		kind: "tool",
		tool: {
			...existing.tool,
			...tool,
			id: existing.tool.id,
			input: tool.input ?? existing.tool.input,
			output: tool.output ?? existing.tool.output,
		},
	};
	const out = parts.slice();
	out[idx] = merged;
	return out;
}

function stripIdSuffix(id: string): string {
	const i = id.lastIndexOf("#");
	return i === -1 ? id : id.slice(0, i);
}
