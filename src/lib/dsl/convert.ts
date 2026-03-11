import type {
	BeatGrid,
	BlendMode as BindingBlendMode,
	PatternArgDef as BindingPatternArgDef,
	PatternSummary,
} from "@/bindings/schema";
import { serialize, serializeGroupExpr } from "./serializer";

/** Minimal annotation shape needed for DSL conversion */
export type AnnotationInput = {
	patternId: number;
	startTime: number;
	endTime: number;
	zIndex: number;
	blendMode: BindingBlendMode;
	args: Record<string, unknown>;
};

import type {
	Annotation,
	Arg,
	ArgValue,
	BlendMode,
	Document,
	GroupExpr,
	PatternDef,
	PatternRegistry,
	Span,
} from "./types";
import { DEFAULT_BLEND_MODE } from "./types";

const ZERO_SPAN: Span = {
	start: { line: 0, column: 0, offset: 0 },
	end: { line: 0, column: 0, offset: 0 },
};

// ── Time ↔ Bar conversion ────────────────────────────────────────

/**
 * Convert a time (seconds) to a fractional bar number (1-indexed).
 * Quantizes to the nearest subdivision.
 */
function timeToBar(time: number, beatGrid: BeatGrid): number {
	const { downbeats, bpm, beatsPerBar } = beatGrid;
	if (downbeats.length === 0) return 1;

	const barDurFallback = (60 / bpm) * beatsPerBar;

	// Handle time before first downbeat: extrapolate backwards
	if (time < downbeats[0] - 1e-6) {
		const barDur =
			downbeats.length >= 2 ? downbeats[1] - downbeats[0] : barDurFallback;
		// How many bars before bar 1?
		const offset = (downbeats[0] - time) / barDur;
		const subsPerBeat = 4;
		const quantize = beatsPerBar * subsPerBeat;
		const snapped = Math.round(offset * quantize) / quantize;
		return 1 - snapped;
	}

	// Find the bar containing this time
	let barIdx = 0;
	for (let i = downbeats.length - 1; i >= 0; i--) {
		if (time >= downbeats[i] - 1e-6) {
			barIdx = i;
			break;
		}
	}

	const barStart = downbeats[barIdx];
	const barEnd =
		barIdx + 1 < downbeats.length
			? downbeats[barIdx + 1]
			: barStart + barDurFallback;
	const barDuration = barEnd - barStart;

	if (barDuration <= 0) return barIdx + 1;

	const fraction = (time - barStart) / barDuration;
	// Quantize to nearest subdivision (beatsPerBar * subsPerBeat grid positions)
	const subsPerBeat = 4; // sixteenth-note resolution, matching serializer default
	const quantize = beatsPerBar * subsPerBeat;
	const snapped = Math.round(fraction * quantize) / quantize;

	return barIdx + 1 + snapped;
}

/**
 * Convert a fractional bar number (1-indexed) to a time (seconds).
 */
function barToTime(bar: number, beatGrid: BeatGrid): number {
	const { downbeats, bpm, beatsPerBar } = beatGrid;
	const totalBars = downbeats.length;

	const wholeBar = Math.floor(bar);
	const fraction = bar - wholeBar;
	const idx = wholeBar - 1; // 0-indexed

	let barStart: number;
	if (idx < 0) {
		// Before bar 1: extrapolate backwards from first downbeat
		const barDur =
			totalBars >= 2 ? downbeats[1] - downbeats[0] : (60 / bpm) * beatsPerBar;
		barStart = downbeats[0] + idx * barDur;
	} else if (idx < totalBars) {
		barStart = downbeats[idx];
	} else {
		// Extrapolate past the last bar
		const lastBarStart = downbeats[totalBars - 1];
		const barDur =
			totalBars >= 2
				? downbeats[totalBars - 1] - downbeats[totalBars - 2]
				: (60 / bpm) * beatsPerBar;
		barStart = lastBarStart + (idx - (totalBars - 1)) * barDur;
	}

	if (fraction === 0) return barStart;

	// Compute bar duration for fractional interpolation
	const nextIdx = wholeBar; // 0-indexed for next bar
	let barEnd: number;
	if (nextIdx < totalBars) {
		barEnd = downbeats[nextIdx];
	} else {
		const barDur =
			totalBars >= 2
				? downbeats[totalBars - 1] - downbeats[totalBars - 2]
				: (60 / bpm) * beatsPerBar;
		barEnd = barStart + barDur;
	}

	return barStart + fraction * (barEnd - barStart);
}

// ── Export: annotations → DSL text ───────────────────────────────

/**
 * Convert track annotations to DSL text.
 *
 * Each annotation becomes one line with its own bar range.
 * Annotations are grouped by z-index (layer), separated by blank lines.
 * Within each layer, annotations are sorted by start time.
 *
 * Returns both the DSL text and a z-index map for faithful reimport.
 */
export function annotationsToDsl(
	annotations: AnnotationInput[],
	beatGrid: BeatGrid,
	patterns: PatternSummary[],
	patternArgs: Record<number, BindingPatternArgDef[]>,
): string {
	if (annotations.length === 0 || beatGrid.downbeats.length === 0) {
		return "";
	}

	const registry = buildRegistry(patterns, patternArgs);
	const patternNameMap = new Map(patterns.map((p) => [p.id, p.name]));

	// Convert each annotation to a DSL Annotation
	const dslAnnotations: { zIndex: number; annotation: Annotation }[] = [];

	for (const ann of annotations) {
		const patternName = patternNameMap.get(ann.patternId);
		if (!patternName) continue;

		const argDefs = patternArgs[ann.patternId] ?? [];
		const rawArgs = (ann.args ?? {}) as Record<string, unknown>;

		// Extract selection expression
		let selection: GroupExpr = { type: "group", name: "all" };
		for (const def of argDefs) {
			if (def.argType === "Selection") {
				const val = rawArgs[def.id];
				if (val && typeof val === "object" && "expression" in val) {
					const expr = (val as { expression: string }).expression;
					if (expr) {
						selection = parseGroupExprString(expr);
					}
				}
				break;
			}
		}

		// Convert non-Selection args — only args that are explicitly present
		const args: Arg[] = [];
		for (const def of argDefs) {
			if (def.argType === "Selection") continue;
			const val = rawArgs[def.id];
			if (val == null) continue;

			const converted = convertArgValue(def.argType, val);
			if (!converted) continue;

			args.push({ key: def.name, value: converted, span: ZERO_SPAN });
		}

		// Compute fractional bar range
		const startBar = timeToBar(ann.startTime, beatGrid);
		let endBar = timeToBar(ann.endTime, beatGrid);

		// Ensure end > start: if quantization collapses them, nudge end forward by one subdivision
		if (endBar <= startBar) {
			const subsPerBeat = 4;
			const subStep = 1 / (beatGrid.beatsPerBar * subsPerBeat);
			endBar = startBar + subStep;
		}

		dslAnnotations.push({
			zIndex: ann.zIndex,
			annotation: {
				type: "annotation",
				pattern: patternName,
				selection,
				range: { start: startBar, end: endBar },
				args,
				blend: (ann.blendMode as BlendMode) ?? DEFAULT_BLEND_MODE,
				span: ZERO_SPAN,
			},
		});
	}

	// Group by z-index, sort layers ascending, sort annotations within each layer by start bar
	const layerMap = new Map<number, Annotation[]>();
	for (const { zIndex, annotation } of dslAnnotations) {
		let layer = layerMap.get(zIndex);
		if (!layer) {
			layer = [];
			layerMap.set(zIndex, layer);
		}
		layer.push(annotation);
	}

	const sortedZIndices = [...layerMap.keys()].sort((a, b) => a - b);
	const layers: Annotation[][] = sortedZIndices.map((z) => {
		const layer = layerMap.get(z)!;
		layer.sort((a, b) => a.range.start - b.range.start);
		return layer;
	});

	const doc: Document = { layers };
	return serialize(doc, registry, { beatsPerBar: beatGrid.beatsPerBar });
}

// ── Import: DSL document → annotations ───────────────────────────

export type DslAnnotation = {
	patternId: number;
	startTime: number;
	endTime: number;
	zIndex: number;
	blendMode: BlendMode;
	args: Record<string, unknown>;
};

/**
 * Convert a parsed DSL document to annotation data.
 *
 * Each annotation line maps to one DslAnnotation.
 * z-index is the layer index (0 for first group, 1 for second, etc).
 */
export function dslToAnnotations(
	document: Document,
	beatGrid: BeatGrid,
	patterns: PatternSummary[],
	patternArgs: Record<number, BindingPatternArgDef[]>,
): DslAnnotation[] {
	if (document.layers.length === 0 || beatGrid.downbeats.length === 0) {
		return [];
	}

	// Build name→id map, preferring the lowest ID for duplicate names
	const patternIdMap = new Map<string, number>();
	for (const p of patterns) {
		if (!patternIdMap.has(p.name)) {
			patternIdMap.set(p.name, p.id);
		}
	}

	const result: DslAnnotation[] = [];

	for (let zIndex = 0; zIndex < document.layers.length; zIndex++) {
		for (const annotation of document.layers[zIndex]) {
			const patternId = patternIdMap.get(annotation.pattern);
			if (patternId === undefined) continue;

			const argDefs = patternArgs[patternId] ?? [];
			const args = convertAnnotationArgs(annotation, argDefs);

			const startTime = barToTime(annotation.range.start, beatGrid);
			const endTime = barToTime(annotation.range.end, beatGrid);

			result.push({
				patternId,
				startTime,
				endTime,
				zIndex,
				blendMode: annotation.blend,
				args,
			});
		}
	}

	return result;
}

function convertAnnotationArgs(
	annotation: Annotation,
	argDefs: BindingPatternArgDef[],
): Record<string, unknown> {
	const args: Record<string, unknown> = {};

	for (const def of argDefs) {
		if (def.argType === "Selection") {
			const exprStr = serializeGroupExpr(annotation.selection);
			args[def.id] = { expression: exprStr, spatialReference: "global" };
			continue;
		}

		const dslArg = annotation.args.find((a) => a.key === def.name);
		if (dslArg) {
			args[def.id] = convertArgValueToAnnotation(dslArg.value, def.argType);
		}
	}

	return args;
}

function convertArgValueToAnnotation(
	value: ArgValue,
	argType: string,
): unknown {
	if (value.type === "color" && argType === "Color") {
		return hexToRgba(value.hex);
	}
	if (value.type === "number" && argType === "Scalar") {
		return value.value;
	}
	return value.type === "number"
		? value.value
		: value.type === "color"
			? value.hex
			: value.value;
}

// ── Helpers ──────────────────────────────────────────────────────

function convertArgValue(argType: string, value: unknown): ArgValue | null {
	if (argType === "Color") {
		if (typeof value === "object" && value !== null && "r" in value) {
			const { r, g, b, a } = value as {
				r: number;
				g: number;
				b: number;
				a?: number;
			};
			return { type: "color", hex: rgbaToHex(r, g, b, a) };
		}
		return null;
	}

	if (argType === "Scalar") {
		if (typeof value === "number") {
			return { type: "number", value };
		}
		return null;
	}

	return null;
}

function rgbaToHex(r: number, g: number, b: number, a?: number): string {
	const rh = Math.round(Math.max(0, Math.min(255, r)))
		.toString(16)
		.padStart(2, "0");
	const gh = Math.round(Math.max(0, Math.min(255, g)))
		.toString(16)
		.padStart(2, "0");
	const bh = Math.round(Math.max(0, Math.min(255, b)))
		.toString(16)
		.padStart(2, "0");
	if (a != null && Math.abs(a - 1) > 1e-6) {
		const ah = Math.round(Math.max(0, Math.min(255, a * 255)))
			.toString(16)
			.padStart(2, "0");
		return `#${rh}${gh}${bh}${ah}`;
	}
	return `#${rh}${gh}${bh}`;
}

export function hexToRgba(hex: string): {
	r: number;
	g: number;
	b: number;
	a: number;
} {
	const clean = hex.replace(/^#/, "");
	const r = Number.parseInt(clean.slice(0, 2), 16);
	const g = Number.parseInt(clean.slice(2, 4), 16);
	const b = Number.parseInt(clean.slice(4, 6), 16);
	const a =
		clean.length >= 8 ? Number.parseInt(clean.slice(6, 8), 16) / 255 : 1;
	return { r, g, b, a };
}

// Keep the old hexToRgb export for compatibility
export function hexToRgb(hex: string): { r: number; g: number; b: number } {
	const clean = hex.replace(/^#/, "");
	const r = Number.parseInt(clean.slice(0, 2), 16);
	const g = Number.parseInt(clean.slice(2, 4), 16);
	const b = Number.parseInt(clean.slice(4, 6), 16);
	return { r, g, b };
}

export function buildRegistry(
	patterns: PatternSummary[],
	patternArgs: Record<number, BindingPatternArgDef[]>,
): PatternRegistry {
	// When there are duplicate pattern names, prefer the lowest ID (first registered)
	const seen = new Set<string>();
	const defs: PatternDef[] = [];
	for (const p of patterns) {
		if (seen.has(p.name)) continue;
		seen.add(p.name);
		const args = (patternArgs[p.id] ?? []).map((a) => ({
			id: a.id,
			name: a.name,
			argType: a.argType,
			defaultValue: convertDefaultValue(a.argType, a.defaultValue),
		}));
		defs.push({ name: p.name, args });
	}
	return new Map(defs.map((d) => [d.name, d]));
}

function convertDefaultValue(argType: string, defaultValue: unknown): unknown {
	if (defaultValue == null) return null;

	if (argType === "Color") {
		if (
			typeof defaultValue === "object" &&
			defaultValue !== null &&
			"r" in defaultValue
		) {
			const { r, g, b } = defaultValue as {
				r: number;
				g: number;
				b: number;
			};
			return rgbaToHex(r, g, b);
		}
		if (typeof defaultValue === "string") return defaultValue;
		return null;
	}

	if (argType === "Scalar") {
		if (typeof defaultValue === "number") return defaultValue;
		return null;
	}

	return null;
}

// ── Minimal group expression parser ────────────────────────────────

export function parseGroupExprString(input: string): GroupExpr {
	let pos = 0;

	function skipWS() {
		while (pos < input.length && input[pos] === " ") pos++;
	}

	function parseFallback(): GroupExpr {
		let left = parseOr();
		skipWS();
		while (pos < input.length && input[pos] === ">") {
			pos++;
			skipWS();
			const right = parseOr();
			left = { type: "fallback", left, right };
			skipWS();
		}
		return left;
	}

	function parseOr(): GroupExpr {
		let left = parseXor();
		skipWS();
		while (pos < input.length && input[pos] === "|") {
			pos++;
			skipWS();
			const right = parseXor();
			left = { type: "or", left, right };
			skipWS();
		}
		return left;
	}

	function parseXor(): GroupExpr {
		let left = parseAnd();
		skipWS();
		while (pos < input.length && input[pos] === "^") {
			pos++;
			skipWS();
			const right = parseAnd();
			left = { type: "xor", left, right };
			skipWS();
		}
		return left;
	}

	function parseAnd(): GroupExpr {
		let left = parseUnary();
		skipWS();
		while (pos < input.length && input[pos] === "&") {
			pos++;
			skipWS();
			const right = parseUnary();
			left = { type: "and", left, right };
			skipWS();
		}
		return left;
	}

	function parseUnary(): GroupExpr {
		skipWS();
		if (pos < input.length && input[pos] === "~") {
			pos++;
			const operand = parseUnary();
			return { type: "not", operand };
		}
		return parsePrimary();
	}

	function parsePrimary(): GroupExpr {
		skipWS();
		if (pos < input.length && input[pos] === "(") {
			pos++;
			const inner = parseFallback();
			skipWS();
			if (pos < input.length && input[pos] === ")") pos++;
			return { type: "paren", inner };
		}
		let name = "";
		while (pos < input.length && /[a-zA-Z0-9_]/.test(input[pos])) {
			name += input[pos];
			pos++;
		}
		return { type: "group", name: name || "all" };
	}

	return parseFallback();
}
