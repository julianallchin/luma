import { createOpenRouter } from "@openrouter/ai-sdk-provider";
import { invoke } from "@tauri-apps/api/core";
import { generateText, tool } from "ai";
import { z } from "zod";
import type { PatchedFixture } from "@/bindings/fixtures";
import type { FixtureGroupNode } from "@/bindings/groups";
import type {
	AnnotationPreview,
	BeatGrid,
	BlendMode,
	PatternArgDef,
	PatternSummary,
} from "@/bindings/schema";
import type { TimelineAnnotation } from "../stores/use-track-editor-store";
import { getOpenRouterKey, VENUE_EXPERT_MODEL } from "./openrouter-key";
import { patternGraphToText } from "./pattern-graph-text";
import { previewToPngBase64 } from "./preview-image";
import {
	createAnnotationMutation,
	deleteAnnotationMutation,
	type MutationContext,
	updateAnnotationMutation,
} from "./score-mutations";
import {
	barToTime,
	findOverlappingClip,
	formatAt,
	formatNormal,
	formatSummary,
	lowestFreeZ,
	timeToBar,
} from "./score-view";

/** What the agent's tools need to read/mutate. The editor passes a snapshot
 * derived from its live store; background sessions pass their own
 * bootstrapped context. Either way the tool definitions are identical. */
export type ToolsContext = {
	trackId: string | null;
	venueId: string | null;
	scoreId: string | null;
	readOnly: boolean;
	durationSeconds: number;
	beatGrid: BeatGrid | null;
	annotations: TimelineAnnotation[];
	patterns: PatternSummary[];
	patternArgs: Record<string, PatternArgDef[]>;
};

export type ToolsBindings = {
	getContext: () => ToolsContext | null;
	/** Called after every successful mutation with the refreshed annotations
	 * list. The caller decides whether to mirror it into the editor store,
	 * the session cache, or both. */
	setAnnotations: (annotations: TimelineAnnotation[]) => void;
};

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

function requireMutationContext(
	ctx: ToolsContext | null,
): MutationContext | { error: string } {
	if (!ctx) return { error: "No session context loaded." };
	if (ctx.readOnly) return { error: "Score is read-only." };
	if (!ctx.scoreId || !ctx.trackId) {
		return { error: "Track or score not loaded." };
	}
	return {
		scoreId: ctx.scoreId,
		trackId: ctx.trackId,
		annotations: ctx.annotations,
		patterns: ctx.patterns,
		patternArgs: ctx.patternArgs,
	};
}

/** Build the tool set bound to a context provider. The same definitions are
 * used by interactive (editor-store-backed) and background (session-backed)
 * runs — only the source of `getContext` differs. */
export function buildAgentTools({ getContext, setAnnotations }: ToolsBindings) {
	const get = (): ToolsContext => getContext() ?? emptyContext();

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
			"Render the score in a bar range as a text view. Without a range, returns a summary of the full track. Use this to see what's already placed before making edits. Bar ranges are inclusive on both ends — startBar=17, lastBar=24 covers bars 17 through 24 (8 bars).",
		inputSchema: z.object({
			startBar: z
				.number()
				.optional()
				.describe(
					"Start bar (1-indexed, inclusive). The window opens at this bar's downbeat. Omit for full track.",
				),
			lastBar: z
				.number()
				.optional()
				.describe(
					"Last bar of the window, inclusive. lastBar=startBar = 1-bar window. Omit for full track.",
				),
			detail: z
				.enum(["summary", "normal"])
				.optional()
				.describe(
					"summary = composition-merged regions, full track. normal = per-clip detail with args, requires a range.",
				),
		}),
		execute: async ({ startBar, lastBar, detail }) => {
			const state = get();
			const wantsNormal = detail === "normal";
			if (wantsNormal && (startBar === undefined || lastBar === undefined)) {
				return { error: "detail=normal requires startBar and lastBar." };
			}
			if (wantsNormal && startBar !== undefined && lastBar !== undefined) {
				if (lastBar < startBar) {
					return { error: "lastBar must be >= startBar (inclusive)." };
				}
				return {
					view: formatNormal(
						state.annotations,
						state.beatGrid,
						state.durationSeconds,
						startBar,
						lastBar + 1,
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
			"Place a new clip on the timeline. ALWAYS call read_pattern on this patternId in an earlier step and observe the result before calling place_clip — you need the arg schema to set args correctly, and parallel calls in one step don't see each other's output. Don't place from a search hit alone. Bar ranges are inclusive on both ends — startBar=17, lastBar=24 covers bars 17 through 24 (8 bars). By default the clip is placed on the lowest existing layer where its time range is free, keeping the stack compact. Use the optional `place` field to override.",
		inputSchema: z.object({
			patternId: z.string(),
			startBar: z
				.number()
				.describe(
					"Start bar (1-indexed, inclusive). The clip starts at this bar's downbeat.",
				),
			lastBar: z
				.number()
				.describe(
					"Last bar of the clip, inclusive. lastBar=startBar = 1-bar clip. Must be >= startBar.",
				),
			blendMode: blendModeEnum.optional().describe("Defaults to 'replace'."),
			place: placeSchema,
			args: argsRecord,
		}),
		execute: async (input) => {
			const state = get();
			const mut = requireMutationContext(state);
			if ("error" in mut) return { error: mut.error };
			if (!state.beatGrid) {
				return { error: "Beat grid not loaded; cannot place clip by bars." };
			}
			if (input.lastBar < input.startBar) {
				return { error: "lastBar must be >= startBar (inclusive)." };
			}
			const startTime = barToTime(input.startBar, state.beatGrid);
			const endTime = barToTime(input.lastBar + 1, state.beatGrid);

			const zResult = resolvePlacement(
				state.annotations,
				input.place,
				startTime,
				endTime,
			);
			if ("error" in zResult) return { error: zResult.error };

			const result = await createAnnotationMutation(mut, {
				patternId: input.patternId,
				startTime,
				endTime,
				zIndex: zResult.z,
				blendMode: (input.blendMode ?? "replace") as BlendMode,
				args: input.args ?? undefined,
			});
			if (!result.value) return { error: "Failed to create clip." };
			setAnnotations(result.annotations);
			const created = result.value;
			return {
				id: created.id,
				patternId: created.patternId,
				startBar: timeToBar(created.startTime, state.beatGrid),
				lastBar: timeToBar(created.endTime, state.beatGrid) - 1,
				z: created.zIndex,
				blendMode: created.blendMode,
			};
		},
	});

	const updateClip = tool({
		description:
			"Update an existing clip's timing, blend mode, or args. If you're changing args, ALWAYS call read_pattern on the clip's patternId in an earlier step and observe the result before calling update_clip — you need the arg schema, and parallel calls in one step don't see each other's output. Bar ranges are inclusive on both ends — startBar=17, lastBar=24 covers bars 17 through 24. To move a clip to a different stack layer, use restack_clip.",
		inputSchema: z.object({
			id: z.string().describe("Clip id."),
			startBar: z
				.number()
				.optional()
				.describe(
					"New start bar (1-indexed, inclusive). The clip starts at this bar's downbeat.",
				),
			lastBar: z
				.number()
				.optional()
				.describe("New last bar of the clip, inclusive. Must be >= startBar."),
			blendMode: blendModeEnum.optional(),
			args: argsRecord,
		}),
		execute: async ({ id, startBar, lastBar, blendMode, args }) => {
			const state = get();
			const mut = requireMutationContext(state);
			if ("error" in mut) return { error: mut.error };
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
				lastBar !== undefined
					? barToTime(lastBar + 1, state.beatGrid)
					: existing.endTime;
			if (newEnd <= newStart) {
				return { error: "lastBar must be >= startBar (inclusive)." };
			}

			// If timing is changing, validate no overlap on the same layer.
			if (startBar !== undefined || lastBar !== undefined) {
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

			const result = await updateAnnotationMutation(mut, {
				id,
				startTime: startBar !== undefined ? newStart : undefined,
				endTime: lastBar !== undefined ? newEnd : undefined,
				blendMode: blendMode ?? null,
				args,
			});
			if (!result.value) return { error: "Update failed." };
			setAnnotations(result.annotations);
			const updated = result.value;
			return {
				id: updated.id,
				startBar: timeToBar(updated.startTime, state.beatGrid),
				lastBar: timeToBar(updated.endTime, state.beatGrid) - 1,
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
			const mut = requireMutationContext(state);
			if ("error" in mut) return { error: mut.error };
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

			const result = await updateAnnotationMutation(mut, {
				id,
				zIndex: zResult.z,
			});
			if (!result.value) return { error: "Restack failed." };
			setAnnotations(result.annotations);
			return { id: result.value.id, z: result.value.zIndex };
		},
	});

	const previewPattern = tool({
		description:
			"Render a small space-time heatmap of a pattern at a bar range, before placing it. Use this to check whether a pattern's behavior fits a section. Bar ranges are inclusive on both ends. Image: rows = fixtures (sorted by activation time), cols = time, brightness = dimmer × RGB. Selection args resolve to all fixtures.",
		inputSchema: z.object({
			patternId: z.string(),
			startBar: z.number().describe("Start bar (1-indexed, inclusive)."),
			lastBar: z
				.number()
				.describe(
					"Last bar of the window, inclusive. lastBar=startBar = 1-bar window. Must be >= startBar.",
				),
		}),
		execute: async ({ patternId, startBar, lastBar }) => {
			const state = get();
			if (!state.beatGrid) {
				return { error: "Beat grid not loaded; cannot resolve bars." };
			}
			if (!state.trackId || !state.venueId) {
				return { error: "Track or venue not loaded." };
			}
			if (lastBar < startBar) {
				return { error: "lastBar must be >= startBar (inclusive)." };
			}
			const startTime = barToTime(startBar, state.beatGrid);
			const endTime = barToTime(lastBar + 1, state.beatGrid);

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
			"Render a heatmap of the *composited* track output (all clips blended) over a bar range. Bar ranges are inclusive on both ends. Use this after placing or editing clips to verify the blend looks right. Reads the live composite cache; if no composite exists yet, error mentions how to trigger one. Same heatmap semantics as preview_pattern.",
		inputSchema: z.object({
			startBar: z.number().describe("Start bar (1-indexed, inclusive)."),
			lastBar: z
				.number()
				.describe(
					"Last bar of the window, inclusive. lastBar=startBar = 1-bar window. Must be >= startBar.",
				),
		}),
		execute: async ({ startBar, lastBar }) => {
			const state = get();
			if (!state.beatGrid) {
				return { error: "Beat grid not loaded; cannot resolve bars." };
			}
			if (!state.trackId) {
				return { error: "Track not loaded." };
			}
			if (lastBar < startBar) {
				return { error: "lastBar must be >= startBar (inclusive)." };
			}
			const startTime = barToTime(startBar, state.beatGrid);
			const endTime = barToTime(lastBar + 1, state.beatGrid);

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
			const mut = requireMutationContext(state);
			if ("error" in mut) return { error: mut.error };
			const result = await deleteAnnotationMutation(mut, id);
			setAnnotations(result.annotations);
			return result.value ? { ok: true, id } : { error: "Delete failed." };
		},
	});

	const askVenue = tool({
		description: `Ask a venue expert a natural-language question about this venue's physical setup. The expert knows every fixture (id, name, type — par/led_bar/moving_head/etc., xyz position in meters, facing direction) and every group (snake_case name + which fixtures it contains). Use it whenever you need to ground a lighting decision in the actual rig — don't guess at group names.

Use ask_venue to:
- Discover what groups exist before writing a Selection ("what groups are there for the front wash?", "is there a group of only uplighters?")
- Pick a selection expression for a specific role ("which groups make a good foundation wash?", "which groups are movers on the back wall?", "what should I select for a strobe accent?")
- Resolve spatial intents ("which groups are pointing up?", "what's on stage left?")
- Sanity-check before placing a clip ("does this venue have any pixel bars I could use for a chase?")

The expert returns plain prose. Quote the exact group names from the answer when setting Selection args on a pattern. Ask focused questions — one intent per call. Call ask_venue early in a section, then reuse the answer.`,
		inputSchema: z.object({
			question: z
				.string()
				.describe(
					"A single, focused question about the venue's fixtures or groups.",
				),
		}),
		execute: async ({ question }) => {
			const state = get();
			if (!state.venueId) {
				return { error: "No venue loaded; cannot consult the venue expert." };
			}
			const apiKey = getOpenRouterKey();
			if (!apiKey) {
				return { error: "OpenRouter API key is not set." };
			}

			let fixtures: PatchedFixture[];
			let groups: FixtureGroupNode[];
			try {
				[fixtures, groups] = await Promise.all([
					invoke<PatchedFixture[]>("get_patched_fixtures", {
						venueId: state.venueId,
					}),
					invoke<FixtureGroupNode[]>("get_grouped_hierarchy", {
						venueId: state.venueId,
					}),
				]);
			} catch (err) {
				return { error: `Failed to load venue data: ${String(err)}` };
			}

			const venueDump = formatVenueContext(fixtures, groups);
			const openrouter = createOpenRouter({
				apiKey,
				appName: "Luma",
				appUrl: "https://luma.show",
			});

			try {
				const result = await generateText({
					model: openrouter(VENUE_EXPERT_MODEL),
					system: `You are a venue expert for a lighting design tool. You receive a dump of every fixture and every group in a venue, then answer one question from another agent who is composing a lighting score.

Coordinate system: Z-up, meters. +X = stage right, +Y = back of venue, -Y = audience/front, +Z = up. Facing labels (up/down/front/back/left/right) describe the fixture's beam direction after rotation; raw Euler angles in radians are also given for precision.

Fixture types: par_wash (basic wash light), pixel_bar (linear pixel strip), moving_head (motorized beam), scanner, strobe, static, unknown.

Answer in plain prose. Always quote the exact snake_case group names so the caller can paste them into a Selection expression. If the venue has nothing matching the question, say so plainly — don't fabricate groups. If the question is ambiguous, give the best literal read and note the ambiguity. Be terse: the caller is mid-task and just needs the answer.`,
					prompt: `${venueDump}\n\nQuestion: ${question}`,
				});
				return { answer: result.text };
			} catch (err) {
				return { error: `Venue expert call failed: ${String(err)}` };
			}
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
		ask_venue: askVenue,
	};
}

function emptyContext(): ToolsContext {
	return {
		trackId: null,
		venueId: null,
		scoreId: null,
		readOnly: false,
		durationSeconds: 0,
		beatGrid: null,
		annotations: [],
		patterns: [],
		patternArgs: {},
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

/**
 * Compact text dump of every fixture + every group for the venue expert.
 * Designed to fit comfortably in context for a typical venue (≤a few hundred
 * fixtures); large venues will produce a longer dump but should still be fine
 * for Kimi 2.6's window.
 */
function formatVenueContext(
	fixtures: PatchedFixture[],
	groups: FixtureGroupNode[],
): string {
	const fixtureLabel = new Map<string, string>();
	for (const f of fixtures) {
		fixtureLabel.set(f.id, f.label ?? `${f.manufacturer} ${f.model}`);
	}

	const fixtureLines = fixtures.map((f) => {
		const name = fixtureLabel.get(f.id) ?? "<unnamed>";
		const pos = `(${fmtN(f.posX)}, ${fmtN(f.posY)}, ${fmtN(f.posZ)})`;
		const rot = `(${fmtN(f.rotX)}, ${fmtN(f.rotY)}, ${fmtN(f.rotZ)})`;
		const facing = facingLabel(f.rotX, f.rotY, f.rotZ);
		const type = inferFixtureTypeFromGroups(f.id, groups);
		return `${f.id} | "${name}" | ${type} | pos=${pos}m | facing=${facing} | rot=${rot}rad`;
	});

	const groupLines = groups.map((g) => {
		const name = g.groupName ?? "<unnamed>";
		const members = g.fixtures.map((m) => `${m.id} ("${m.label}")`).join(", ");
		const axes: string[] = [];
		if (g.axisLr !== null) axes.push(`LR=${fmtN(g.axisLr)}`);
		if (g.axisFb !== null) axes.push(`FB=${fmtN(g.axisFb)}`);
		if (g.axisAb !== null) axes.push(`AB=${fmtN(g.axisAb)}`);
		const axesStr = axes.length > 0 ? `  [${axes.join(" ")}]` : "";
		return `${name} (${g.fixtureType}, ${g.fixtures.length} fixtures)${axesStr}\n    ${members || "<empty>"}`;
	});

	return `# Venue context

## Fixtures (${fixtures.length})
Format: id | name | type | position (x,y,z meters) | facing | rotation (x,y,z radians)
${fixtureLines.join("\n")}

## Groups (${groups.length})
Format: snake_case_name (type, count)  [optional axis positions, normalized −1..+1]
    members listed as id ("label")
${groupLines.join("\n")}

## Selection expressions
Selections in patterns reference group names with boolean ops: \`front_wash\`, \`front_wash & left_movers\`, \`drum_uplighters | dj_booth > back_wash\`. The literal \`all\` selects every fixture.`;
}

function fmtN(n: number): string {
	return Number.isFinite(n) ? n.toFixed(2) : "?";
}

/**
 * Find which group a fixture belongs to to infer its type, since
 * PatchedFixture doesn't carry the auto-detected FixtureType directly.
 * Falls back to "unknown" if the fixture isn't in any group.
 */
function inferFixtureTypeFromGroups(
	fixtureId: string,
	groups: FixtureGroupNode[],
): string {
	for (const g of groups) {
		for (const f of g.fixtures) {
			if (f.id === fixtureId) return f.fixtureType;
		}
	}
	return "unknown";
}

/**
 * Coarse human-readable facing label from Euler angles in radians.
 * Default fixture-local forward is +Y (Z-up, Y-forward). Applies intrinsic
 * X → Y → Z rotations, then picks the dominant axis of the resulting vector.
 * Venue convention: +X = right, +Y = back, -Y = front, +Z = up.
 */
function facingLabel(rotX: number, rotY: number, rotZ: number): string {
	const sx = Math.sin(rotX);
	const cx = Math.cos(rotX);
	const sy = Math.sin(rotY);
	const cy = Math.cos(rotY);
	const sz = Math.sin(rotZ);
	const cz = Math.cos(rotZ);
	const dx = cz * sy * sx - sz * cx;
	const dy = sz * sy * sx + cz * cx;
	const dz = cy * sx;
	const ax = Math.abs(dx);
	const ay = Math.abs(dy);
	const az = Math.abs(dz);
	if (az >= ax && az >= ay) return dz > 0 ? "up" : "down";
	if (ay >= ax) return dy > 0 ? "back" : "front";
	return dx > 0 ? "right" : "left";
}
