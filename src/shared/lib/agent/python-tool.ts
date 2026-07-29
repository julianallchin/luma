import { tool } from "ai";
import { z } from "zod";
import type { ToolLabel } from "@/shared/components/agent-chat/parts";
import { invoke } from "@/shared/lib/tauri";

/**
 * The `python` tool — a persistent Python workspace over the agent thread's
 * read-only Luma bindings.
 *
 * The model supplies only `code`. Workspace, thread, binding revision, scope
 * ids and the graph snapshot are all resolved here from the live editor
 * bridge, never by the model (design §7.1).
 */

// TODO(bindings): swap to the generated `PythonCellResult` / `PythonFigure` /
// `PythonScopeInput` from `@/bindings/schema` once the Rust models land.

/** Scope resolved by the calling agent and handed to the binding providers. */
export type PythonScopeInput = {
	trackId?: string | null;
	venueId?: string | null;
	scoreId?: string | null;
	patternId?: string | null;
	/** Absolute track seconds [start, end]. */
	window?: [number, number] | null;
	/** Graph-shaped JSON — the graph agent's working canvas. */
	graphDefinition?: unknown;
};

export type PythonFigure = {
	artifactRel: string;
	width: number;
	height: number;
	base64Png: string;
};

export type PythonCellResult = {
	status: "ok" | "error" | "interrupted" | "failed";
	stdout: string;
	stderr: string;
	repr: string | null;
	traceback: string | null;
	figures: PythonFigure[];
	notices: string[];
	durationMs: number;
};

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

const DESCRIPTION = `Execute Python in a namespace that persists for this agent thread. Read-only Luma bindings live under \`luma\` and are refreshed before every call; the variables, functions and imports you create persist across calls. numpy, scipy, librosa and matplotlib are available. You get back stdout, stderr, the last expression's value, a traceback when it fails, and any matplotlib figures as images you can actually see. Write normal cell-shaped Python — no wrapper function, no \`return\`.

Orientation:
- Call \`luma.catalog()\` first — it lists the available paths, tensor shapes, axes, units and provenance, plus what is unavailable and why.
- \`luma.audio\` is audio signal (mix, stems). \`luma.features\` is what was derived from audio (beats, downbeats, drum onsets, bar classifications, chords, waveform bands). Neither is a fallback for the other: pick the branch that matches the question.
- Tensors expose \`.values\` (numpy), \`.shape\`, \`.axes\`, \`.times_s\`, \`.unit\`, \`.provenance\`. Keyed families are dict-style: \`luma.features.drum_onsets["kick"]\`, \`luma.audio.stems["drums"]\`.
- \`luma.graph.run.views\` holds the latest graph run's view-node output when a graph run is in scope. All times are absolute track seconds.
- Plot with matplotlib: every figure still open at the end of the cell is captured and returned to you as an image.
- Application state CANNOT be modified from Python — it is for measurement and analysis. Make changes with the normal graph or score tools, then observe the refreshed result with Python.`;

export function buildPythonTool({
	threadId,
	getScope,
	abortSignal,
}: {
	threadId: string;
	getScope: () => PythonScopeInput | null;
	abortSignal?: AbortSignal;
}) {
	return tool({
		description: DESCRIPTION,
		inputSchema: z.object({
			code: z.string().describe("Python cell source."),
		}),
		execute: async ({ code }): Promise<PythonToolOutput> => {
			const scope = getScope() ?? {};

			// Aborting the turn interrupts the running cell, but we still await the
			// backend's terminal result (it comes back as `interrupted`), so the
			// transcript records what the cell managed to emit.
			const cancel = () => {
				void invoke<boolean>("cancel_python_cell", { threadId }).catch(
					() => {},
				);
			};
			if (abortSignal?.aborted) cancel();
			abortSignal?.addEventListener("abort", cancel, { once: true });

			let result: PythonCellResult;
			try {
				result = await invoke<PythonCellResult>("run_python_cell", {
					threadId,
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
			}

			return toStoredOutput(result);
		},
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

/** Chat-row label for a python run: the first meaningful line of code, plus a
 * status marker when the cell did not simply succeed. */
export function pythonToolLabel(tool: {
	input: unknown;
	output: unknown;
}): ToolLabel {
	const snippet = pythonCodeSnippet(tool.input);
	const output = tool.output as Partial<PythonToolOutput> | undefined;
	const status = output?.status;
	if (!status || status === "ok") return { verb: "python", detail: snippet };
	const seconds = output?.durationMs
		? ` ${(output.durationMs / 1000).toFixed(1)}s`
		: "";
	return {
		verb: "python",
		detail: [snippet, `${status}${seconds}`].filter(Boolean).join(" · "),
	};
}

/** Short label detail for a python tool run: the first meaningful line of code. */
export function pythonCodeSnippet(input: unknown, max = 48): string | null {
	const code = (input as { code?: unknown } | undefined)?.code;
	if (typeof code !== "string") return null;
	const line = code
		.split("\n")
		.map((l) => l.trim())
		.find((l) => l.length > 0 && !l.startsWith("#"));
	if (!line) return null;
	return line.length > max ? `${line.slice(0, max - 1)}…` : line;
}
