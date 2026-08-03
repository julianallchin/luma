import type { TrackScore } from "@/bindings/schema";
import { createAgentChat } from "@/shared/components/agent-chat/create-agent-chat";
import type { ToolVocab } from "@/shared/components/agent-chat/parts";
import { renderPythonToolDetail } from "@/shared/components/agent-chat/python-tool-detail";
import { lumaLanguageModel } from "@/shared/lib/agent/openrouter";
import {
	buildPythonTool,
	pythonToolLabel,
} from "@/shared/lib/agent/python-tool";
import { invoke } from "@/shared/lib/tauri";
import type { TimelineAnnotation } from "../stores/use-track-editor-store";
import { useTrackEditorStore } from "../stores/use-track-editor-store";
import { buildSystemPrompt } from "./build-context";
import { OPENROUTER_MODEL } from "./openrouter-key";
import type { SessionContext } from "./track-session-store";
import {
	type TrackSessionScope,
	toTimelineAnnotations,
	useTrackSessionStore,
} from "./track-session-store";

/** Luma's live handle on the track being scored. The durable task owns the
 * Python workspace; this bridge only mirrors committed rows into a mounted
 * editor (or the background session cache). */
export type TrackBridge = TrackSessionScope & {
	getContext: () => SessionContext | null;
	refreshAnnotations: () => Promise<void>;
};

function createModel(modelId = OPENROUTER_MODEL) {
	return lumaLanguageModel(modelId);
}

const VOCAB: ToolVocab = {
	verbs: {
		python: { running: "Running", past: "Ran", noun: "python cell" },
	},
	formatLabel: pythonToolLabel,
	renderers: { python: renderPythonToolDetail },
};

function buildSystem(bridge: TrackBridge): string {
	const context = bridge.getContext();
	if (!context) return "No track context is loaded yet.";
	return buildSystemPrompt({
		trackName: context.trackName,
		durationSeconds: context.durationSeconds,
		beatGrid: context.beatGrid,
		venueName: context.venueName,
	});
}

export const trackAgent = createAgentChat<TrackBridge>({
	agentKind: "track_copilot",
	subjectKind: "track",
	validateThreadScope: ({ subjectKey, init, bridge }) => {
		const { venueId, scoreId } = init;
		if (!venueId || !scoreId) {
			throw new Error(
				"Track agent requires a venue and score before opening a conversation.",
			);
		}
		if (
			bridge &&
			(bridge.trackId !== subjectKey ||
				bridge.venueId !== venueId ||
				bridge.scoreId !== scoreId)
		) {
			throw new Error(
				"Track agent bridge does not match its immutable conversation scope.",
			);
		}
	},
	createModel,
	notConfiguredMessage: "OpenRouter API key is not set.",
	vocab: VOCAB,
	reasoningEffort: "medium",
	buildSystem,
	buildTools: ({ getBridge, threadId, turnMessageId, abortSignal }) => ({
		python: buildPythonTool({
			threadId,
			turnMessageId,
			abortSignal,
			getScope: () => {
				const bridge = getBridge();
				const context = bridge?.getContext();
				if (!bridge || !context) return null;
				return {
					trackId: bridge.trackId,
					venueId: bridge.venueId,
					scoreId: bridge.scoreId,
				};
			},
			afterExecute: async () => {
				await getBridge()?.refreshAnnotations();
			},
		}),
	}),
	applyAuthoredState: async ({ result, bridge }) => {
		if (result.document.kind !== "track_score") {
			throw new Error("The authored revision is not a track score.");
		}
		await bridge.refreshAnnotations();
	},
	refreshAuthoredState: ({ bridge }) => bridge.refreshAnnotations(),
});

function publishAnnotations(
	scope: TrackSessionScope,
	annotations: TimelineAnnotation[],
): void {
	useTrackSessionStore.getState().setAnnotations(scope, annotations);
}

/** Build a bridge pinned to one immutable track/venue/score scope. Interactive
 * and background sessions use the same path; the latter simply has no mounted
 * editor to mirror into. */
export function trackBridge(scope: TrackSessionScope): TrackBridge {
	const { trackId, venueId, scoreId } = scope;
	const captured = { trackId, venueId, scoreId } as const;
	let refreshIntent = 0;
	return {
		...captured,
		getContext: () => useTrackSessionStore.getState().getContext(captured),
		refreshAnnotations: async () => {
			const intent = ++refreshIntent;
			const context = useTrackSessionStore.getState().getContext(captured);
			if (!context) return;
			const editor = useTrackEditorStore.getState();
			if (
				editor.trackId === trackId &&
				editor.venueId === venueId &&
				editor.scoreId === scoreId
			) {
				await editor.reloadAnnotations();
				if (intent !== refreshIntent) return;
				const current = useTrackEditorStore.getState();
				if (
					current.trackId === trackId &&
					current.venueId === venueId &&
					current.scoreId === scoreId
				) {
					publishAnnotations(captured, current.annotations);
					return;
				}
			}
			const rows = await invoke<TrackScore[]>("list_track_scores", {
				scoreId,
			});
			if (intent !== refreshIntent) return;
			publishAnnotations(
				captured,
				toTimelineAnnotations(rows, context.patterns),
			);
		},
	};
}
