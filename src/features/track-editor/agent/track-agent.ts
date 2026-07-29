import { createAgentChat } from "@/shared/components/agent-chat/create-agent-chat";
import type { ToolView, ToolVocab } from "@/shared/components/agent-chat/parts";
import { lumaOpenRouter } from "@/shared/lib/agent/openrouter";
import type { TimelineAnnotation } from "../stores/use-track-editor-store";
import { useTrackEditorStore } from "../stores/use-track-editor-store";
import { buildSystemPrompt, formatBarTags } from "./build-context";
import { OPENROUTER_MODEL } from "./openrouter-key";
import { formatSummary } from "./score-view";
import { buildAgentTools } from "./tools";
import type { SessionContext } from "./track-session-store";
import { useTrackSessionStore } from "./track-session-store";

/** The track copilot's live handle on the track being scored. Registered per
 * trackId (see `use-track-agent.ts`); tools resolve it lazily so a long-lived
 * thread always acts on the current score. */
export type TrackBridge = {
	trackId: string;
	getContext: () => SessionContext | null;
	setAnnotations: (annotations: TimelineAnnotation[]) => void;
};

function createModel() {
	return lumaOpenRouter()?.(OPENROUTER_MODEL) ?? null;
}

// Track-agent tool vocabulary, consumed by the shared renderer.
const TOOL_VERB: Record<string, { past: string; noun: string | null }> = {
	search_patterns: { past: "Searched", noun: "pattern" },
	read_pattern: { past: "Read", noun: "pattern" },
	view_score: { past: "Viewed", noun: "score" },
	view_at: { past: "Viewed", noun: "moment" },
	preview_pattern: { past: "Previewed", noun: "pattern" },
	view_blended_result: { past: "Viewed blend", noun: "range" },
	place_clip: { past: "Placed", noun: "clip" },
	update_clip: { past: "Updated", noun: "clip" },
	restack_clip: { past: "Restacked", noun: "clip" },
	delete_clip: { past: "Deleted", noun: "clip" },
	// noun=null: verb already implies its object ("Asked venue").
	ask_venue: { past: "Asked venue", noun: null },
};

/** Pattern names for tool labels. Read from the editor store at label time —
 * the vocab is static, and names don't change mid-conversation. */
function patternName(id: string): string | undefined {
	return useTrackEditorStore.getState().patterns.find((p) => p.id === id)?.name;
}

function formatToolLabel(tool: ToolView): {
	verb: string;
	detail: string | null;
} {
	const meta = TOOL_VERB[tool.name];
	const verb = meta?.past ?? tool.name;
	switch (tool.name) {
		case "search_patterns": {
			const input = tool.input as { query?: string } | undefined;
			const q = input?.query?.trim();
			return { verb, detail: q ? `"${q}" patterns` : "all patterns" };
		}
		case "read_pattern": {
			const input = tool.input as { patternId?: string } | undefined;
			const output = tool.output as { name?: string } | undefined;
			const name =
				output?.name ??
				(input?.patternId ? patternName(input.patternId) : undefined);
			return { verb, detail: name ?? null };
		}
		case "view_score": {
			const input = tool.input as
				| { startBar?: number; lastBar?: number; detail?: string }
				| undefined;
			if (input?.startBar !== undefined && input.lastBar !== undefined) {
				return {
					verb: "Viewed score",
					detail: `bars ${input.startBar}–${input.lastBar}`,
				};
			}
			return { verb: "Viewed score", detail: input?.detail ?? "summary" };
		}
		case "view_at": {
			const input = tool.input as { bar?: number } | undefined;
			return {
				verb: "Viewed stack",
				detail: input?.bar !== undefined ? `bar ${input.bar}` : null,
			};
		}
		case "preview_pattern": {
			const input = tool.input as
				| { patternId?: string; startBar?: number; lastBar?: number }
				| undefined;
			const name = input?.patternId ? patternName(input.patternId) : undefined;
			const range =
				input?.startBar !== undefined && input.lastBar !== undefined
					? `bars ${input.startBar}–${input.lastBar}`
					: null;
			const detail = [name, range].filter(Boolean).join(" · ") || null;
			return { verb: "Previewed pattern", detail };
		}
		case "view_blended_result": {
			const input = tool.input as
				| { startBar?: number; lastBar?: number }
				| undefined;
			return {
				verb: "Viewed blend",
				detail:
					input?.startBar !== undefined && input.lastBar !== undefined
						? `bars ${input.startBar}–${input.lastBar}`
						: null,
			};
		}
		case "place_clip": {
			const input = tool.input as { patternId?: string } | undefined;
			const name = input?.patternId ? patternName(input.patternId) : undefined;
			return { verb: "Placed clip", detail: name ?? null };
		}
		case "update_clip":
			return { verb: "Updated clip", detail: null };
		case "restack_clip":
			return { verb: "Restacked clip", detail: null };
		case "delete_clip":
			return { verb: "Deleted clip", detail: null };
		case "ask_venue": {
			const input = tool.input as { question?: string } | undefined;
			const q = input?.question?.trim();
			return { verb: "Asked venue", detail: q ? `"${truncate(q, 60)}"` : null };
		}
		default:
			return { verb, detail: null };
	}
}

function truncate(s: string, max: number): string {
	return s.length > max ? `${s.slice(0, max - 1)}…` : s;
}

const VOCAB: ToolVocab = { verbs: TOOL_VERB, formatLabel: formatToolLabel };

function buildSystem(bridge: TrackBridge): string {
	const ctx = bridge.getContext();
	if (!ctx) return "No track context is loaded yet.";
	return `${buildSystemPrompt({
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
}

export const trackAgent = createAgentChat<TrackBridge>({
	agentKind: "track_copilot",
	subjectKind: "track",
	createModel,
	notConfiguredMessage: "OpenRouter API key is not set.",
	vocab: VOCAB,
	reasoningEffort: "medium",
	buildSystem,
	buildTools: ({ getBridge }) =>
		buildAgentTools({
			getContext: () => {
				const bridge = getBridge();
				const ctx = bridge?.getContext();
				if (!bridge || !ctx) return null;
				return {
					trackId: bridge.trackId,
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
			setAnnotations: (annotations) => getBridge()?.setAnnotations(annotations),
		}),
});

/** Build the bridge for a track. The same shape backs the interactive sidebar
 * and the background auto-light driver — only who calls `bootstrap()` differs. */
export function trackBridge(trackId: string): TrackBridge {
	return {
		trackId,
		getContext: () => useTrackSessionStore.getState().getContext(trackId),
		setAnnotations: (annotations) => {
			useTrackSessionStore.getState().setAnnotations(trackId, annotations);
			// If the editor is open for this same track, push the new annotations
			// in so the visualizer/timeline reflect agent edits live. Skip the
			// reload — we already have the fresh list.
			if (useTrackEditorStore.getState().trackId === trackId) {
				useTrackEditorStore.setState({ annotations });
			}
		},
	};
}
