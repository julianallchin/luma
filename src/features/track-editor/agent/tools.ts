import { invoke } from "@tauri-apps/api/core";
import { tool } from "ai";
import { z } from "zod";
import type {
	BlendMode,
	PatternArgDef,
	PatternSummary,
} from "@/bindings/schema";
import type { useTrackEditorStore } from "../stores/use-track-editor-store";
import { patternGraphToText } from "./pattern-graph-text";

type Store = typeof useTrackEditorStore;

const blendModeEnum = z.enum([
	"replace",
	"add",
	"multiply",
	"screen",
	"max",
	"min",
	"lighten",
	"value",
	"subtract",
]);

const argsRecord = z
	.record(z.string(), z.unknown())
	.optional()
	.describe(
		"Arbitrary pattern args. Use read_pattern first to learn the schema.",
	);

/** Build the tool set bound to the live track editor store. */
export function buildAgentTools(store: Store) {
	const get = () => store.getState();

	const searchPatterns = tool({
		description:
			"Search the user's pattern library by name, description, or category. Returns up to 20 matches.",
		inputSchema: z.object({
			query: z
				.string()
				.describe("Free-text search. Empty string lists all patterns."),
		}),
		execute: async ({ query }) => {
			const patterns = get().patterns;
			const q = query.trim().toLowerCase();
			const filtered = q
				? patterns.filter((p) => matchPattern(p, q))
				: patterns.slice();
			return {
				count: filtered.length,
				patterns: filtered.slice(0, 20).map((p) => ({
					id: p.id,
					name: p.name,
					category: p.categoryName,
					description: p.description,
				})),
			};
		},
	});

	const readPattern = tool({
		description:
			"Read a pattern's node graph and arg schema. Returns a compact text representation of the graph.",
		inputSchema: z.object({
			patternId: z.string().describe("Pattern id from search_patterns."),
		}),
		execute: async ({ patternId }) => {
			const state = get();
			const pattern = state.patterns.find((p) => p.id === patternId);
			if (!pattern) {
				return { error: `Unknown patternId: ${patternId}` };
			}
			let graphText = "<no graph>";
			try {
				const graphJson = await invoke<string>("get_pattern_graph", {
					id: patternId,
				});
				graphText = patternGraphToText(graphJson);
			} catch (err) {
				graphText = `<failed to load graph: ${String(err)}>`;
			}
			const argDefs = state.patternArgs[patternId] ?? [];
			return {
				id: pattern.id,
				name: pattern.name,
				description: pattern.description,
				category: pattern.categoryName,
				args: argDefs.map(formatArgDef),
				graph: graphText,
			};
		},
	});

	const placeAnnotation = tool({
		description:
			"Place a new annotation (a pattern placement) on the timeline. zIndex defaults to one above the highest existing.",
		inputSchema: z.object({
			patternId: z.string(),
			startTime: z.number().describe("Start time in seconds."),
			endTime: z.number().describe("End time in seconds. Must be > startTime."),
			zIndex: z
				.number()
				.optional()
				.describe(
					"Stacking layer. Higher draws on top. Omit to auto-pick the next free row.",
				),
			blendMode: blendModeEnum.optional(),
			args: argsRecord,
		}),
		execute: async ({
			patternId,
			startTime,
			endTime,
			zIndex,
			blendMode,
			args,
		}) => {
			const state = get();
			if (state.readOnly) return { error: "Score is read-only." };
			if (endTime <= startTime) {
				return { error: "endTime must be greater than startTime." };
			}
			const resolvedZ =
				typeof zIndex === "number"
					? zIndex
					: nextFreeZ(state.annotations.map((a) => a.zIndex));

			const created = await state.createAnnotation({
				patternId,
				startTime,
				endTime,
				zIndex: resolvedZ,
				blendMode: (blendMode ?? "replace") as BlendMode,
				args: args ?? undefined,
			});
			if (!created) return { error: "Failed to create annotation." };
			return {
				id: created.id,
				patternId: created.patternId,
				startTime: created.startTime,
				endTime: created.endTime,
				zIndex: created.zIndex,
				blendMode: created.blendMode,
			};
		},
	});

	const updateAnnotation = tool({
		description:
			"Update an existing annotation's timing, layer, blend mode, or args.",
		inputSchema: z.object({
			id: z.string().describe("Annotation id."),
			startTime: z.number().optional(),
			endTime: z.number().optional(),
			zIndex: z.number().optional(),
			blendMode: blendModeEnum.optional(),
			args: argsRecord,
		}),
		execute: async ({ id, startTime, endTime, zIndex, blendMode, args }) => {
			const state = get();
			if (state.readOnly) return { error: "Score is read-only." };
			const existing = state.annotations.find((a) => a.id === id);
			if (!existing) return { error: `Unknown annotation id: ${id}` };
			const updated = await state.updateAnnotation({
				id,
				startTime,
				endTime,
				zIndex,
				blendMode: blendMode ?? null,
				args,
			});
			if (!updated) return { error: "Update failed." };
			return {
				id: updated.id,
				startTime: updated.startTime,
				endTime: updated.endTime,
				zIndex: updated.zIndex,
				blendMode: updated.blendMode,
			};
		},
	});

	const deleteAnnotation = tool({
		description: "Delete an annotation by id.",
		inputSchema: z.object({
			id: z.string().describe("Annotation id."),
		}),
		execute: async ({ id }) => {
			const state = get();
			if (state.readOnly) return { error: "Score is read-only." };
			const ok = await state.deleteAnnotation(id);
			return ok ? { ok: true, id } : { error: "Delete failed." };
		},
	});

	return {
		search_patterns: searchPatterns,
		read_pattern: readPattern,
		place_annotation: placeAnnotation,
		update_annotation: updateAnnotation,
		delete_annotation: deleteAnnotation,
	};
}

function matchPattern(p: PatternSummary, q: string): boolean {
	if (p.name.toLowerCase().includes(q)) return true;
	if (p.description?.toLowerCase().includes(q)) return true;
	if (p.categoryName?.toLowerCase().includes(q)) return true;
	return false;
}

function formatArgDef(arg: PatternArgDef) {
	return {
		id: arg.id,
		name: arg.name,
		type: arg.argType,
		default: arg.defaultValue,
	};
}

function nextFreeZ(zs: number[]): number {
	if (zs.length === 0) return 0;
	return Math.max(...zs) + 1;
}
