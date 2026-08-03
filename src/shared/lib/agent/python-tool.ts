import { tool } from "ai";
import { z } from "zod";
import type { PythonCellResult, PythonScopeInput } from "@/bindings/schema";
import type { ToolLabel } from "@/shared/components/agent-chat/parts";
import { invoke } from "@/shared/lib/tauri";

/**
 * The `python` tool — a persistent Python workspace over the agent thread's
 * Luma bindings and any semantic capabilities the current agent owns.
 *
 * The model supplies a human-readable purpose and the code. Workspace, thread,
 * binding revision, scope ids and the graph snapshot are all resolved here
 * from the live editor bridge, never by the model (design §7.1).
 */

/**
 * What an agent knows about its own scope. Every field of the wire-level
 * `PythonScopeInput` is nullable, and each agent only knows some of them (the
 * track assistant has no pattern, the graph agent has no score), so callers give
 * us a partial and `execute` fills the rest with the nulls Rust expects.
 */
export type PythonScope = Partial<PythonScopeInput>;

/** A figure as stored in the transcript. `base64Png` is dropped when a single
 * figure is too heavy to persist (the UI then shows a placeholder). */
export type StoredFigure = {
	width: number;
	height: number;
	base64Png?: string;
};

export type PythonToolOutput = {
	status: PythonCellResult["status"];
	stdout: string;
	stderr: string;
	repr: string | null;
	traceback: string | null;
	notices: string[];
	figureCount: number;
	figures: StoredFigure[];
	durationMs: number;
};

/** Above this, a single figure's base64 is not persisted in the transcript. */
const MAX_PERSISTED_FIGURE_BYTES = 2_000_000;
/** Above this total, remaining figures are not sent to the model. */
const MAX_MODEL_FIGURE_BYTES = 6_000_000;

const DESCRIPTION = `Execute Python in a namespace that persists for this agent thread. Current Luma state lives under \`luma\` and is refreshed before every call; the variables, functions and imports you create persist across calls. numpy, scipy, librosa and matplotlib are available. You get back stdout, stderr, the last expression's value, a traceback when it fails, and any matplotlib figures as images you can actually see. Write normal cell-shaped Python — no wrapper function, no \`return\`.

For every call, describe the cell's purpose before its code. Use a noun phrase of no more than four words that completes “Running …”, for example “section energy analysis.” Do not restate the code.

Orientation:
- Inspect the branch relevant to the question. \`luma.catalog()\` is available when you need the full inventory, but do not dump it by default.
- In a track thread, \`luma.track\` is both the current score and its guarded edit surface. Stage a complete candidate with \`edit = luma.track.edit()\`. Add with \`edit.add_clip(pattern, bars=(start, end), z=0, blend="replace", args={...}, selection="group_expression")\`; update those same fields with \`edit.update_clip(clip_id, ...)\`; remove with \`edit.remove_clip(clip_id)\`. Use \`seconds=(start, end)\` instead of \`bars=...\` when appropriate. Inspect with \`edit.diff()\`, \`edit.check()\`, and an explicit half-open \`view = edit.window(...)\`; \`view.timeline()\` shows authored clips and \`view.output.heatmap()\` shows the actual composited candidate. Only \`edit.apply()\` mutates the live score.
- \`luma.audio\` is audio signal (mix, stems). \`luma.features\` is what was derived from audio (beats, downbeats, drum onsets, bar classifications, chords, waveform bands). Neither is a fallback for the other: pick the branch that matches the question.
- Tensors expose \`.values\` (numpy), \`.shape\`, \`.axes\`, \`.times_s\`, \`.unit\`, \`.provenance\`. Keyed families are dict-style: \`luma.features.drum_onsets["kick"]\`, \`luma.audio.stems["drums"]\`.
- \`luma.graph.run.views\` holds the latest graph run's view-node output when a graph run is in scope. All times are absolute track seconds.
- Plot with matplotlib: every figure still open at the end of the cell is captured and returned to you as an image.`;

/** Cells in one thread share a single kernel, so they must run one at a time.
 * The model may issue several python calls in one step; chain them here so the
 * second waits instead of bouncing off the backend's already-running guard.
 * Only settled (never rejected) promises are stored as queue tails. */
const cellQueues = new Map<string, Promise<unknown>>();

function enqueueCell<T>(threadId: string, run: () => Promise<T>): Promise<T> {
	const prev = cellQueues.get(threadId) ?? Promise.resolve();
	const result = prev.then(run);
	cellQueues.set(
		threadId,
		result.then(
			() => undefined,
			() => undefined,
		),
	);
	return result;
}

export function buildPythonTool({
	threadId,
	turnMessageId,
	getScope,
	abortSignal,
	afterExecute,
}: {
	threadId: string;
	/** Already persisted before the remote model call begins. */
	turnMessageId: string;
	getScope: () => PythonScope | null;
	abortSignal?: AbortSignal;
	/** Refresh caller-owned UI state after a real cell result. The callback is
	 * deliberately outside the model result; persisted application state stays
	 * authoritative even if the editor was not mounted during the edit. */
	afterExecute?: () => void | Promise<void>;
}) {
	return tool({
		description: DESCRIPTION,
		inputSchema: z.object({
			purpose: z
				.string()
				.trim()
				.min(1)
				.max(80)
				.refine(
					(value) => value.trim().split(/\s+/).length <= 4,
					"Purpose must be four words or fewer.",
				)
				.describe(
					'A noun phrase of no more than four words describing the intended outcome; it must read naturally after "Running".',
				),
			code: z.string().describe("Python cell source."),
		}),
		execute: ({ code }): Promise<PythonToolOutput> =>
			enqueueCell(threadId, async () => {
				if (abortSignal?.aborted) {
					return {
						status: "interrupted",
						stdout: "",
						stderr: "",
						repr: null,
						traceback: null,
						notices: ["Python cell was stopped before it started."],
						figureCount: 0,
						figures: [],
						durationMs: 0,
					};
				}

				const partial = getScope() ?? {};
				const scope: PythonScopeInput = {
					trackId: partial.trackId ?? null,
					venueId: partial.venueId ?? null,
					scoreId: partial.scoreId ?? null,
					patternId: partial.patternId ?? null,
					implementationId: partial.implementationId ?? null,
					window: partial.window ?? null,
					graphDefinition: partial.graphDefinition ?? null,
				};

				// Aborting the turn interrupts the running cell, but we still await the
				// backend's terminal result (it comes back as `interrupted`), so the
				// transcript records what the cell managed to emit.
				const cancel = () => {
					void invoke<boolean>("cancel_python_cell", { threadId }).catch(
						() => {},
					);
				};
				abortSignal?.addEventListener("abort", cancel, { once: true });

				let result: PythonCellResult;
				try {
					result = await invoke<PythonCellResult>("run_python_cell", {
						threadId,
						turnMessageId,
						code,
						scope,
					});
				} catch (err) {
					return {
						status: "failed",
						stdout: "",
						stderr: "",
						repr: null,
						traceback: null,
						notices: [`Python workspace unavailable: ${String(err)}`],
						figureCount: 0,
						figures: [],
						durationMs: 0,
					};
				} finally {
					abortSignal?.removeEventListener("abort", cancel);
					// The host may have committed even when the local IPC response was
					// lost. Always re-read caller-owned state after an attempted cell.
					try {
						await afterExecute?.();
					} catch (err) {
						console.error("[python-tool] post-cell refresh failed:", err);
					}
				}

				return toStoredOutput(result);
			}),
		toModelOutput: ({ output }) =>
			pythonModelOutput(output as PythonToolOutput),
	});
}

/** Shape the wire result into what the transcript keeps. Figure base64 is kept
 * (the chat UI renders it) but a single oversized figure is dropped. */
export function toStoredOutput(result: PythonCellResult): PythonToolOutput {
	const figures = result.figures ?? [];
	return {
		status: result.status,
		stdout: result.stdout ?? "",
		stderr: result.stderr ?? "",
		repr: result.repr ?? null,
		traceback: result.traceback ?? null,
		notices: result.notices ?? [],
		figureCount: figures.length,
		figures: figures.map((f) => ({
			width: f.width,
			height: f.height,
			...(f.base64Png && f.base64Png.length <= MAX_PERSISTED_FIGURE_BYTES
				? { base64Png: f.base64Png }
				: {}),
		})),
		durationMs: result.durationMs ?? 0,
	};
}

type ModelContent =
	| { type: "text"; text: string }
	| { type: "image-data"; data: string; mediaType: string };

/** Notebook-native model output (design §15): one text block assembling
 * notices / stdout / stderr / traceback / repr, then one image per figure. */
export function pythonModelOutput(output: PythonToolOutput): {
	type: "content";
	value: ModelContent[];
} {
	const sections: string[] = [];
	for (const notice of output.notices) sections.push(`note: ${notice}`);
	if (output.stdout.trim().length > 0) {
		sections.push(`stdout:\n${output.stdout.replace(/\n+$/, "")}`);
	}
	if (output.stderr.trim().length > 0) {
		sections.push(`stderr:\n${output.stderr.replace(/\n+$/, "")}`);
	}
	if (output.traceback) sections.push(output.traceback.replace(/\n+$/, ""));
	if (output.repr) sections.push(output.repr);

	if (output.status === "interrupted") {
		sections.push("Cell interrupted before it finished.");
	} else if (sections.length === 0 && output.figureCount === 0) {
		sections.push("(no output)");
	}

	const value: ModelContent[] = [];
	const text = sections.join("\n\n");
	if (text.length > 0) value.push({ type: "text", text });

	let budget = MAX_MODEL_FIGURE_BYTES;
	let omitted = 0;
	for (const figure of output.figures) {
		const data = figure.base64Png;
		if (!data || data.length > budget) {
			omitted += 1;
			continue;
		}
		budget -= data.length;
		value.push({ type: "image-data", data, mediaType: "image/png" });
	}
	if (omitted > 0) {
		value.push({
			type: "text",
			text: `note: ${omitted} further figure(s) were too large to include. Plot fewer or smaller figures per cell.`,
		});
	}

	return { type: "content", value };
}

/** Chat-row label for a Python run: the model-authored purpose, plus a status
 * marker when the cell did not simply succeed. */
export function pythonToolLabel(tool: {
	input: unknown;
	output: unknown;
}): ToolLabel {
	const purpose = (tool.input as { purpose?: unknown } | undefined)?.purpose;
	const detail =
		typeof purpose === "string" && purpose.trim() ? purpose.trim() : null;
	const output = tool.output as Partial<PythonToolOutput> | undefined;
	const status = output?.status;
	if (!status || status === "ok") return { verb: "python", detail };
	const seconds = output?.durationMs
		? ` ${(output.durationMs / 1000).toFixed(1)}s`
		: "";
	return {
		verb: "python",
		detail: [detail, `${status}${seconds}`].filter(Boolean).join(" · "),
	};
}
