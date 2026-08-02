import type {
	BeatGrid,
	BlendMode as BindingBlendMode,
	PatternArgDef as BindingPatternArgDef,
	PatternSummary,
} from "@/bindings/schema";
import { serialize, serializeGroupExpr } from "./serializer";

/** Minimal annotation shape needed for DSL conversion */
export type AnnotationInput = {
	id?: string;
	patternId: string;
	startTime: number;
	endTime: number;
	zIndex: number;
	blendMode: BindingBlendMode;
	args: Record<string, unknown>;
};

export type DslExportOptions = {
	/** Omit only clip identities when presenting an exemplar for new authoring. */
	includeClipIds?: boolean;
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
 * Convert a time (seconds) to a fractional bar number (1-indexed) without
 * quantization. Callers decide whether that position is authorable on the
 * configured musical subdivision.
 */
function timeToBar(time: number, beatGrid: BeatGrid): number {
	const { downbeats, bpm, beatsPerBar } = beatGrid;
	if (downbeats.length === 0) return 1;

	const barDurFallback = (60 / bpm) * beatsPerBar;

	// Handle time before first downbeat: extrapolate backwards
	if (time < downbeats[0] - 1e-6) {
		const barDur =
			downbeats.length >= 2 ? downbeats[1] - downbeats[0] : barDurFallback;
		const offset = (downbeats[0] - time) / barDur;
		return 1 - offset;
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
	return barIdx + 1 + fraction;
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

function exactBarPosition(
	time: number,
	beatGrid: BeatGrid,
	subsPerBeat = 4,
): number | null {
	if (beatGrid.downbeats.length === 0) return null;
	const raw = timeToBar(time, beatGrid);
	const positionsPerBar = beatGrid.beatsPerBar * subsPerBeat;
	const snapped = Math.round(raw * positionsPerBar) / positionsPerBar;
	return barToTime(snapped, beatGrid) === time ? snapped : null;
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
	patternArgs: Record<string, BindingPatternArgDef[]>,
	options?: DslExportOptions,
): string {
	if (annotations.length === 0) return "";

	const registry = buildRegistry(patterns, patternArgs);
	const patternNameMap = new Map(patterns.map((p) => [p.id, p.name]));

	// Convert each annotation to a DSL Annotation
	const dslAnnotations: { zIndex: number; annotation: Annotation }[] = [];

	for (const ann of annotations) {
		const patternName = patternNameMap.get(ann.patternId);
		if (!patternName) {
			throw new Error(
				`Cannot export score: pattern "${ann.patternId}" is unavailable`,
			);
		}

		const argDefs = patternArgs[ann.patternId] ?? [];
		const rawArgs = (ann.args ?? {}) as Record<string, unknown>;

		// The first explicit, canonical Selection value gets the concise
		// parenthesized syntax. Any unusual/legacy Selection JSON remains a
		// regular argument so no fields are discarded.
		let selection: GroupExpr | null = null;
		let selectionSpatialReference: string | undefined;
		let representedSelectionKey: string | null = null;
		for (const def of argDefs) {
			if (def.argType !== "Selection") continue;
			const value = rawArgs[def.id];
			if (isCanonicalSelection(value)) {
				const parsed = tryParseGroupExprString(value.expression);
				if (parsed === null || serializeGroupExpr(parsed) !== value.expression)
					continue;
				selection = parsed;
				selectionSpatialReference = value.spatialReference;
				representedSelectionKey = def.id;
				break;
			}
		}
		if (!argDefs.some((def) => def.argType === "Selection")) {
			// Existing authoring syntax keeps a target expression even for
			// patterns without a Selection port; it is semantically inert.
			selection = { type: "group", name: "all" };
		}

		// Preserve every persisted argument, including values for removed
		// pattern args. Known values get readable sugar only when that sugar is
		// exactly reversible; otherwise they use JSON.
		const args: Arg[] = [];
		for (const [key, value] of Object.entries(rawArgs)) {
			if (key === representedSelectionKey) continue;
			const def = argDefs.find((candidate) => candidate.id === key);
			args.push({
				key,
				value: convertArgValue(def?.argType, value),
				span: ZERO_SPAN,
			});
		}

		if (
			!Number.isFinite(ann.startTime) ||
			!Number.isFinite(ann.endTime) ||
			ann.endTime <= ann.startTime
		) {
			throw new Error(
				`Cannot export clip "${ann.id ?? patternName}": invalid time range ${ann.startTime}–${ann.endTime}`,
			);
		}

		const startBar = exactBarPosition(ann.startTime, beatGrid);
		const endBar = exactBarPosition(ann.endTime, beatGrid);
		const range =
			startBar !== null && endBar !== null
				? { start: startBar, end: endBar }
				: {
						start: ann.startTime,
						end: ann.endTime,
						unit: "seconds" as const,
					};

		dslAnnotations.push({
			zIndex: ann.zIndex,
			annotation: {
				type: "annotation",
				id: options?.includeClipIds === false ? undefined : ann.id,
				pattern: patternName,
				patternId: ann.patternId,
				selection,
				selectionSpatialReference,
				range,
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
		// biome-ignore lint/style/noNonNullAssertion: key comes from layerMap.keys()
		const layer = layerMap.get(z)!;
		layer.sort((a, b) => a.range.start - b.range.start);
		return layer;
	});

	const doc: Document = { layers, zIndices: sortedZIndices };
	return serialize(doc, registry, { beatsPerBar: beatGrid.beatsPerBar });
}

// ── Import: DSL document → annotations ───────────────────────────

export type DslAnnotation = {
	id?: string;
	patternId: string;
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
	patternArgs: Record<string, BindingPatternArgDef[]>,
): DslAnnotation[] {
	if (document.layers.length === 0) return [];

	const patternsByName = new Map<string, PatternSummary[]>();
	const patternsById = new Map(
		patterns.map((pattern) => [pattern.id, pattern]),
	);
	for (const p of patterns) {
		const sameName = patternsByName.get(p.name) ?? [];
		sameName.push(p);
		patternsByName.set(p.name, sameName);
	}

	const result: DslAnnotation[] = [];

	for (let zIndex = 0; zIndex < document.layers.length; zIndex++) {
		for (const annotation of document.layers[zIndex]) {
			let patternId = annotation.patternId;
			if (patternId !== undefined) {
				if (!patternsById.has(patternId)) {
					throw new Error(
						`Cannot compile clip: pattern id "${patternId}" is unavailable`,
					);
				}
			} else {
				const matches = patternsByName.get(annotation.pattern) ?? [];
				if (matches.length === 0) {
					throw new Error(
						`Cannot compile clip: pattern "${annotation.pattern}" is unavailable`,
					);
				}
				if (matches.length > 1) {
					throw new Error(
						`Cannot compile clip: pattern name "${annotation.pattern}" is ambiguous`,
					);
				}
				patternId = matches[0].id;
			}

			const argDefs = patternArgs[patternId] ?? [];
			const args = convertAnnotationArgs(annotation, argDefs);

			let startTime: number;
			let endTime: number;
			if (annotation.range.unit === "seconds") {
				startTime = annotation.range.start;
				endTime = annotation.range.end;
			} else {
				if (beatGrid.downbeats.length === 0) {
					throw new Error(
						`Cannot compile bar-timed clip "${annotation.id ?? annotation.pattern}" without a beat grid`,
					);
				}
				startTime = barToTime(annotation.range.start, beatGrid);
				endTime = barToTime(annotation.range.end, beatGrid);
			}

			result.push({
				id: annotation.id,
				patternId,
				startTime,
				endTime,
				zIndex: document.zIndices?.[zIndex] ?? zIndex,
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
	const defsById = new Map<string, BindingPatternArgDef>();
	for (const def of argDefs) {
		if (defsById.has(def.id)) {
			throw new Error(
				`Cannot compile clip "${annotation.id ?? annotation.pattern}": pattern interface contains duplicate arg id "${def.id}"`,
			);
		}
		defsById.set(def.id, def);
	}

	for (const dslArg of annotation.args) {
		const def = resolveArgDef(annotation, dslArg.key, argDefs, defsById);
		const key = def?.id ?? dslArg.key;
		if (Object.hasOwn(args, key)) {
			throw new Error(
				`Cannot compile clip "${annotation.id ?? annotation.pattern}": arg "${key}" is assigned more than once`,
			);
		}
		args[key] = convertArgValueToAnnotation(dslArg.value, def?.argType);
	}

	if (annotation.selection !== null) {
		const selectionDef = argDefs.find(
			(def) => def.argType === "Selection" && !(def.id in args),
		);
		if (selectionDef) {
			args[selectionDef.id] = {
				expression: serializeGroupExpr(annotation.selection),
				spatialReference: annotation.selectionSpatialReference ?? "global",
			};
		}
	}

	return args;
}

function resolveArgDef(
	annotation: Annotation,
	key: string,
	argDefs: BindingPatternArgDef[],
	defsById: ReadonlyMap<string, BindingPatternArgDef>,
): BindingPatternArgDef | undefined {
	const exact = defsById.get(key);
	if (exact) return exact;

	const aliases = argDefs.filter((candidate) => candidate.name === key);
	if (aliases.length > 1) {
		const ids = aliases.map((candidate) => `"${candidate.id}"`).join(", ");
		throw new Error(
			`Cannot compile clip "${annotation.id ?? annotation.pattern}": arg name "${key}" is ambiguous; use a stable arg id (${ids})`,
		);
	}
	return aliases[0];
}

function convertArgValueToAnnotation(
	value: ArgValue,
	argType?: string,
): unknown {
	if (value.type === "color" && argType === "Color") {
		return hexToRgba(value.hex);
	}
	if (value.type === "number" && argType === "Scalar") {
		return value.value;
	}
	if (value.type === "json") return value.value;
	return value.type === "number"
		? value.value
		: value.type === "color"
			? value.hex
			: value.value;
}

// ── Helpers ──────────────────────────────────────────────────────

function convertArgValue(
	argType: string | undefined,
	value: unknown,
): ArgValue {
	if (argType === "Color") {
		if (isExactlyHexRepresentableColor(value)) {
			const { r, g, b, a } = value as {
				r: number;
				g: number;
				b: number;
				a: number;
			};
			return { type: "color", hex: rgbaToHex(r, g, b, a) };
		}
		return { type: "json", value: toJsonValue(value) };
	}

	if (argType === "Scalar") {
		if (typeof value === "number") {
			if (!Number.isFinite(value)) {
				throw new Error("Cannot export a non-finite Scalar argument");
			}
			return { type: "number", value };
		}
		return { type: "json", value: toJsonValue(value) };
	}

	return { type: "json", value: toJsonValue(value) };
}

function isCanonicalSelection(
	value: unknown,
): value is { expression: string; spatialReference: string } {
	if (typeof value !== "object" || value === null || Array.isArray(value)) {
		return false;
	}
	const record = value as Record<string, unknown>;
	return (
		Object.keys(record).every(
			(key) => key === "expression" || key === "spatialReference",
		) &&
		Object.keys(record).length === 2 &&
		typeof record.expression === "string" &&
		record.expression.trim().length > 0 &&
		/^[a-zA-Z0-9_~&|^>()\s]+$/.test(record.expression) &&
		typeof record.spatialReference === "string" &&
		/^[a-zA-Z_][a-zA-Z0-9_]*$/.test(record.spatialReference)
	);
}

function isExactlyHexRepresentableColor(value: unknown): boolean {
	if (typeof value !== "object" || value === null || Array.isArray(value)) {
		return false;
	}
	const record = value as Record<string, unknown>;
	if (
		Object.keys(record).length !== 4 ||
		!["r", "g", "b", "a"].every((key) => key in record)
	) {
		return false;
	}
	const { r, g, b, a } = record;
	if (
		![r, g, b, a].every(
			(channel) => typeof channel === "number" && Number.isFinite(channel),
		) ||
		![r, g, b].every(
			(channel) =>
				typeof channel === "number" &&
				Number.isInteger(channel) &&
				channel >= 0 &&
				channel <= 255,
		) ||
		typeof a !== "number" ||
		a < 0 ||
		a > 1
	) {
		return false;
	}
	const roundTrip = hexToRgba(
		rgbaToHex(r as number, g as number, b as number, a),
	);
	return (
		roundTrip.r === r &&
		roundTrip.g === g &&
		roundTrip.b === b &&
		roundTrip.a === a
	);
}

function toJsonValue(value: unknown): import("./types").JsonValue {
	const serialized = JSON.stringify(value);
	if (serialized === undefined) {
		throw new Error("Cannot export a non-JSON score argument");
	}
	const parsed = JSON.parse(serialized) as import("./types").JsonValue;
	if (!jsonValuesEqual(value, parsed)) {
		throw new Error(
			"Cannot export a score argument without changing its value",
		);
	}
	return parsed;
}

function jsonValuesEqual(left: unknown, right: unknown): boolean {
	if (Object.is(left, right)) return true;
	if (Array.isArray(left) && Array.isArray(right)) {
		return (
			left.length === right.length &&
			left.every((value, index) => jsonValuesEqual(value, right[index]))
		);
	}
	if (
		typeof left === "object" &&
		left !== null &&
		!Array.isArray(left) &&
		typeof right === "object" &&
		right !== null &&
		!Array.isArray(right)
	) {
		const leftRecord = left as Record<string, unknown>;
		const rightRecord = right as Record<string, unknown>;
		const leftKeys = Object.keys(leftRecord);
		const rightKeys = Object.keys(rightRecord);
		return (
			leftKeys.length === rightKeys.length &&
			leftKeys.every(
				(key) =>
					Object.hasOwn(rightRecord, key) &&
					jsonValuesEqual(leftRecord[key], rightRecord[key]),
			)
		);
	}
	return false;
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
	patternArgs: Record<string, BindingPatternArgDef[]>,
): PatternRegistry {
	const registry = new Map<string, PatternDef>();
	for (const p of patterns) {
		const bindingArgs = patternArgs[p.id] ?? [];
		const args = bindingArgs.map((a) => ({
			id: a.id,
			// Parser validation uses the same id-or-name lookup as the readable DSL.
			// Hide aliases that cannot resolve uniquely so they can never shadow an
			// exact stable ID; dslToAnnotations reports an authored ambiguous alias.
			name: isSafeArgAlias(a, bindingArgs) ? a.name : a.id,
			argType: a.argType,
			defaultValue: convertDefaultValue(a.argType, a.defaultValue),
		}));
		const def = { id: p.id, name: p.name, args };
		if (!registry.has(p.name)) {
			registry.set(p.name, def);
		} else {
			// Preserve duplicate names for stable-id lookup without changing the
			// public name-keyed registry API.
			registry.set(`${p.name}\u0000${p.id}`, def);
		}
	}
	return registry;
}

function isSafeArgAlias(
	arg: BindingPatternArgDef,
	argDefs: BindingPatternArgDef[],
): boolean {
	return (
		argDefs.filter((candidate) => candidate.name === arg.name).length === 1 &&
		!argDefs.some((candidate) => candidate.id === arg.name)
	);
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
			if (pos >= input.length || input[pos] !== ")") {
				throw new Error("unclosed group expression");
			}
			pos++;
			return { type: "paren", inner };
		}
		let name = "";
		while (pos < input.length && /[a-zA-Z0-9_]/.test(input[pos])) {
			name += input[pos];
			pos++;
		}
		if (!name) throw new Error("expected group name");
		return { type: "group", name };
	}

	const result = parseFallback();
	skipWS();
	if (pos !== input.length) {
		throw new Error(`unexpected selection syntax at offset ${pos}`);
	}
	return result;
}

function tryParseGroupExprString(input: string): GroupExpr | null {
	try {
		return parseGroupExprString(input);
	} catch {
		return null;
	}
}
