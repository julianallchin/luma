import { invoke } from "@tauri-apps/api/core";
import { tool } from "ai";
import { z } from "zod";
import type {
	AnnotationPreview,
	BlendMode,
	PatternArgDef,
	PatternSummary,
} from "@/bindings/schema";
import type { useTrackEditorStore } from "../stores/use-track-editor-store";
import { patternGraphToText } from "./pattern-graph-text";
import { previewToPngBase64 } from "./preview-image";
import {
	barToTime,
	findOverlappingClip,
	formatAt,
	formatNormal,
	formatSummary,
	lowestFreeZ,
	timeToBar,
} from "./score-view";

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

const placeSchema = z
	.union([
		z.literal("top"),
		z.literal("bottom"),
		z.object({ z: z.number().int() }),
		z.object({ sameLayerAs: z.string() }),
	])
	.optional()
	.describe(
		"Where to put the clip in the stack. Omit (default) → lowest existing layer where the clip fits, keeping the stack compact. 'top' → new layer above all others. 'bottom' → new layer below all others. {z:N} → explicit layer (errors if occupied at this time). {sameLayerAs:#id} → reuse another clip's layer (errors if occupied at this time).",
	);

const restackPlaceSchema = z
	.union([
		z.literal("top"),
		z.literal("bottom"),
		z.object({ z: z.number().int() }),
	])
	.describe("Stack target: 'top', 'bottom', or {z:N}.");

/** Build the tool set bound to the live track editor store. */
export function buildAgentTools(store: Store) {
	const get = () => store.getState();

	const searchPatterns = tool({
		description:
			"Search the user's *verified* pattern library by name, description, or category. Returns up to 20 matches. Pass `category` to filter to a specific category (recommended when building a layer with a known role — e.g. category='wash' for a foundation layer). Pass `query` for free-text. Both can be combined; either can be omitted.",
		inputSchema: z.object({
			query: z
				.string()
				.optional()
				.describe(
					"Free-text search over name/description/category. Omit or empty to skip text matching.",
				),
			category: z
				.string()
				.optional()
				.describe(
					"Exact category name (case-insensitive) to filter by. See the category list in the system prompt.",
				),
		}),
		execute: async ({ query, category }) => {
			const verified = get().patterns.filter((p) => p.isVerified);
			const q = query?.trim().toLowerCase() ?? "";
			const cat = category?.trim().toLowerCase() ?? "";
			const filtered = verified.filter((p) => {
				if (cat && p.categoryName?.toLowerCase() !== cat) return false;
				if (q && !matchPattern(p, q)) return false;
				return true;
			});
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

	const viewScore = tool({
		description:
			"Render the score in a bar range as a text view. Without a range, returns a summary of the full track. Use this to see what's already placed before making edits.",
		inputSchema: z.object({
			startBar: z
				.number()
				.optional()
				.describe("Start bar (1-indexed, fractional). Omit for full track."),
			endBar: z.number().optional().describe("End bar. Omit for full track."),
			detail: z
				.enum(["summary", "normal"])
				.optional()
				.describe(
					"summary = composition-merged regions, full track. normal = per-clip detail with args, requires a range.",
				),
		}),
		execute: async ({ startBar, endBar, detail }) => {
			const state = get();
			const wantsNormal = detail === "normal";
			if (wantsNormal && (startBar === undefined || endBar === undefined)) {
				return { error: "detail=normal requires startBar and endBar." };
			}
			if (wantsNormal && startBar !== undefined && endBar !== undefined) {
				if (endBar <= startBar) {
					return { error: "endBar must be greater than startBar." };
				}
				return {
					view: formatNormal(
						state.annotations,
						state.beatGrid,
						state.durationSeconds,
						startBar,
						endBar,
					),
				};
			}
			return {
				view: formatSummary(
					state.annotations,
					state.beatGrid,
					state.durationSeconds,
				),
			};
		},
	});

	const viewAt = tool({
		description:
			"Show the instantaneous stack of clips active at a specific bar (bottom → top).",
		inputSchema: z.object({
			bar: z.number().describe("Bar number (1-indexed, fractional)."),
		}),
		execute: async ({ bar }) => {
			const state = get();
			return {
				view: formatAt(
					state.annotations,
					state.beatGrid,
					state.durationSeconds,
					bar,
				),
			};
		},
	});

	const placeClip = tool({
		description:
			"Place a new clip on the timeline. By default the clip is placed on the lowest existing layer where its time range is free, keeping the stack compact. Use the optional `place` field to override.",
		inputSchema: z.object({
			patternId: z.string(),
			startBar: z.number().describe("Start bar (1-indexed, fractional)."),
			endBar: z.number().describe("End bar. Must be > startBar."),
			blendMode: blendModeEnum.optional().describe("Defaults to 'replace'."),
			place: placeSchema,
			args: argsRecord,
		}),
		execute: async (input) => {
			const state = get();
			if (state.readOnly) return { error: "Score is read-only." };
			if (!state.beatGrid) {
				return { error: "Beat grid not loaded; cannot place clip by bars." };
			}
			if (input.endBar <= input.startBar) {
				return { error: "endBar must be greater than startBar." };
			}
			const startTime = barToTime(input.startBar, state.beatGrid);
			const endTime = barToTime(input.endBar, state.beatGrid);

			const zResult = resolvePlacement(
				state.annotations,
				input.place,
				startTime,
				endTime,
			);
			if ("error" in zResult) return { error: zResult.error };

			const created = await state.createAnnotation({
				patternId: input.patternId,
				startTime,
				endTime,
				zIndex: zResult.z,
				blendMode: (input.blendMode ?? "replace") as BlendMode,
				args: input.args ?? undefined,
			});
			if (!created) return { error: "Failed to create clip." };
			return {
				id: created.id,
				patternId: created.patternId,
				startBar: timeToBar(created.startTime, state.beatGrid),
				endBar: timeToBar(created.endTime, state.beatGrid),
				z: created.zIndex,
				blendMode: created.blendMode,
			};
		},
	});

	const updateClip = tool({
		description:
			"Update an existing clip's timing, blend mode, or args. To move a clip to a different stack layer, use restack_clip.",
		inputSchema: z.object({
			id: z.string().describe("Clip id."),
			startBar: z.number().optional(),
			endBar: z.number().optional(),
			blendMode: blendModeEnum.optional(),
			args: argsRecord,
		}),
		execute: async ({ id, startBar, endBar, blendMode, args }) => {
			const state = get();
			if (state.readOnly) return { error: "Score is read-only." };
			const existing = state.annotations.find((a) => a.id === id);
			if (!existing) return { error: `Unknown clip id: ${id}` };
			if (!state.beatGrid) {
				return { error: "Beat grid not loaded; cannot update by bars." };
			}

			const newStart =
				startBar !== undefined
					? barToTime(startBar, state.beatGrid)
					: existing.startTime;
			const newEnd =
				endBar !== undefined
					? barToTime(endBar, state.beatGrid)
					: existing.endTime;
			if (newEnd <= newStart) {
				return { error: "endBar must be greater than startBar." };
			}

			// If timing is changing, validate no overlap on the same layer.
			if (startBar !== undefined || endBar !== undefined) {
				const conflict = findOverlappingClip(
					state.annotations,
					newStart,
					newEnd,
					existing.zIndex,
					existing.id,
				);
				if (conflict) {
					return {
						error: `Within-layer overlap with #${conflict.id} at z=${existing.zIndex}. Move that clip first or pick a different time range.`,
					};
				}
			}

			const updated = await state.updateAnnotation({
				id,
				startTime: startBar !== undefined ? newStart : undefined,
				endTime: endBar !== undefined ? newEnd : undefined,
				blendMode: blendMode ?? null,
				args,
			});
			if (!updated) return { error: "Update failed." };
			return {
				id: updated.id,
				startBar: timeToBar(updated.startTime, state.beatGrid),
				endBar: timeToBar(updated.endTime, state.beatGrid),
				z: updated.zIndex,
				blendMode: updated.blendMode,
			};
		},
	});

	const restackClip = tool({
		description:
			"Move an existing clip to a different stack layer. Errors if the target layer is already occupied at the clip's time range.",
		inputSchema: z.object({
			id: z.string(),
			place: restackPlaceSchema,
		}),
		execute: async ({ id, place }) => {
			const state = get();
			if (state.readOnly) return { error: "Score is read-only." };
			const existing = state.annotations.find((a) => a.id === id);
			if (!existing) return { error: `Unknown clip id: ${id}` };

			const zResult = resolvePlacement(
				state.annotations,
				place,
				existing.startTime,
				existing.endTime,
				existing.id,
			);
			if ("error" in zResult) return { error: zResult.error };
			if (zResult.z === existing.zIndex) {
				return { id, z: existing.zIndex, noop: true };
			}

			const updated = await state.updateAnnotation({
				id,
				zIndex: zResult.z,
			});
			if (!updated) return { error: "Restack failed." };
			return { id: updated.id, z: updated.zIndex };
		},
	});

	const previewPattern = tool({
		description:
			"Render a small space-time heatmap of a pattern at a bar range, before placing it. Use this to check whether a pattern's behavior fits a section. Image: rows = fixtures (sorted by activation time), cols = time, brightness = dimmer × RGB. Selection args resolve to all fixtures.",
		inputSchema: z.object({
			patternId: z.string(),
			startBar: z.number().describe("Start bar (1-indexed, fractional)."),
			endBar: z.number().describe("End bar. Must be > startBar."),
		}),
		execute: async ({ patternId, startBar, endBar }) => {
			const state = get();
			if (!state.beatGrid) {
				return { error: "Beat grid not loaded; cannot resolve bars." };
			}
			if (!state.trackId || !state.venueId) {
				return { error: "Track or venue not loaded." };
			}
			if (endBar <= startBar) {
				return { error: "endBar must be greater than startBar." };
			}
			const startTime = barToTime(startBar, state.beatGrid);
			const endTime = barToTime(endBar, state.beatGrid);

			let preview: AnnotationPreview;
			try {
				preview = await invoke<AnnotationPreview>("preview_pattern_image", {
					patternId,
					trackId: state.trackId,
					venueId: state.venueId,
					startTime,
					endTime,
					beatGrid: state.beatGrid,
				});
			} catch (err) {
				return { error: String(err) };
			}

			const base64 = await previewToPngBase64(preview);
			return {
				width: preview.width,
				height: preview.height,
				dominantColor: preview.dominantColor,
				base64,
			};
		},
		toModelOutput: ({ output }) => imageToolOutput(output, "Pattern preview"),
	});

	const viewBlendedResult = tool({
		description:
			"Render a heatmap of the *composited* track output (all clips blended) over a bar range. Use this after placing or editing clips to verify the blend looks right. Reads the live composite cache; if no composite exists yet, error mentions how to trigger one. Same heatmap semantics as preview_pattern.",
		inputSchema: z.object({
			startBar: z.number().describe("Start bar (1-indexed, fractional)."),
			endBar: z.number().describe("End bar. Must be > startBar."),
		}),
		execute: async ({ startBar, endBar }) => {
			const state = get();
			if (!state.beatGrid) {
				return { error: "Beat grid not loaded; cannot resolve bars." };
			}
			if (!state.trackId) {
				return { error: "Track not loaded." };
			}
			if (endBar <= startBar) {
				return { error: "endBar must be greater than startBar." };
			}
			const startTime = barToTime(startBar, state.beatGrid);
			const endTime = barToTime(endBar, state.beatGrid);

			let preview: AnnotationPreview;
			try {
				preview = await invoke<AnnotationPreview>("view_composite_image", {
					trackId: state.trackId,
					startTime,
					endTime,
				});
			} catch (err) {
				return { error: String(err) };
			}

			const base64 = await previewToPngBase64(preview);
			return {
				width: preview.width,
				height: preview.height,
				dominantColor: preview.dominantColor,
				base64,
			};
		},
		toModelOutput: ({ output }) => imageToolOutput(output, "Blended composite"),
	});

	const deleteClip = tool({
		description: "Delete a clip by id.",
		inputSchema: z.object({
			id: z.string().describe("Clip id."),
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
		view_score: viewScore,
		view_at: viewAt,
		preview_pattern: previewPattern,
		view_blended_result: viewBlendedResult,
		place_clip: placeClip,
		update_clip: updateClip,
		restack_clip: restackClip,
		delete_clip: deleteClip,
	};
}

type ImageToolOutput =
	| { error: string }
	| {
			width: number;
			height: number;
			dominantColor: [number, number, number];
			base64: string;
	  };

function imageToolOutput(output: unknown, label: string) {
	const o = output as ImageToolOutput;
	if ("error" in o) {
		return { type: "error-text" as const, value: o.error };
	}
	return {
		type: "content" as const,
		value: [
			{
				type: "text" as const,
				text: `${label} (${o.width}×${o.height}). Rows = fixtures sorted by activation time, cols = time, brightness = dimmer × RGB.`,
			},
			{
				type: "image-data" as const,
				data: o.base64,
				mediaType: "image/png",
			},
		],
	};
}

type PlaceArg =
	| "top"
	| "bottom"
	| { z: number }
	| { sameLayerAs: string }
	| undefined;

function resolvePlacement(
	annotations: import("../stores/use-track-editor-store").TimelineAnnotation[],
	place: PlaceArg,
	startTime: number,
	endTime: number,
	excludeId?: string,
): { z: number } | { error: string } {
	if (place === undefined) {
		return { z: lowestFreeZ(annotations, startTime, endTime) };
	}
	if (place === "top") {
		const max = annotations.reduce(
			(m, a) => (a.zIndex > m ? a.zIndex : m),
			Number.NEGATIVE_INFINITY,
		);
		return { z: Number.isFinite(max) ? max + 1 : 0 };
	}
	if (place === "bottom") {
		const min = annotations.reduce(
			(m, a) => (a.zIndex < m ? a.zIndex : m),
			Number.POSITIVE_INFINITY,
		);
		return { z: Number.isFinite(min) ? min - 1 : 0 };
	}
	if ("z" in place) {
		const conflict = findOverlappingClip(
			annotations,
			startTime,
			endTime,
			place.z,
			excludeId,
		);
		if (conflict) {
			return {
				error: `z=${place.z} is occupied by #${conflict.id} (${conflict.patternName ?? conflict.patternId}) at the requested time. Pick a different layer or omit "place" to auto-select the lowest free one.`,
			};
		}
		return { z: place.z };
	}
	if ("sameLayerAs" in place) {
		const target = annotations.find((a) => a.id === place.sameLayerAs);
		if (!target) {
			return { error: `Unknown clip id for sameLayerAs: ${place.sameLayerAs}` };
		}
		const conflict = findOverlappingClip(
			annotations,
			startTime,
			endTime,
			target.zIndex,
			excludeId,
		);
		if (conflict) {
			return {
				error: `Layer z=${target.zIndex} (with #${target.id}) is occupied by #${conflict.id} at the requested time. Pick a different layer.`,
			};
		}
		return { z: target.zIndex };
	}
	return { error: "Invalid `place` argument." };
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
