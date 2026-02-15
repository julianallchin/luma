import type {
	BeatGrid,
	PatternArgDef as BindingPatternArgDef,
	PatternSummary,
} from "@/bindings/schema";
import type { TimelineAnnotation } from "@/features/track-editor/stores/use-track-editor-store";
import { serialize, serializeTagExpr } from "./serializer";
import type {
	Arg,
	ArgValue,
	BarBlock,
	BlendMode,
	Document,
	PatternDef,
	PatternLayer,
	PatternRegistry,
	Span,
	TagExpr,
} from "./types";
import { DEFAULT_BLEND_MODE } from "./types";

const ZERO_SPAN: Span = {
	start: { line: 0, column: 0, offset: 0 },
	end: { line: 0, column: 0, offset: 0 },
};

/**
 * Convert track annotations to DSL text.
 *
 * Maps each annotation's time range to bar numbers via the beat grid,
 * groups consecutive identical bars, and applies hold detection.
 */
export function annotationsToDsl(
	annotations: TimelineAnnotation[],
	beatGrid: BeatGrid,
	patterns: PatternSummary[],
	patternArgs: Record<number, BindingPatternArgDef[]>,
): string {
	if (annotations.length === 0 || beatGrid.downbeats.length === 0) {
		return "";
	}

	const registry = buildRegistry(patterns, patternArgs);
	const patternNameMap = new Map(patterns.map((p) => [p.id, p.name]));
	const totalBars = beatGrid.downbeats.length;

	// Compute layers for each bar
	const barLayersList: PatternLayer[][] = [];
	for (let bar = 1; bar <= totalBars; bar++) {
		const barStart = beatGrid.downbeats[bar - 1];
		const barEnd =
			bar < totalBars ? beatGrid.downbeats[bar] : Number.POSITIVE_INFINITY;

		// Find overlapping annotations, sorted by zIndex (lower first)
		const overlapping = annotations
			.filter((a) => a.startTime < barEnd && a.endTime > barStart)
			.sort((a, b) => a.zIndex - b.zIndex);

		const layers = overlapping
			.map((a) => annotationToLayer(a, patternNameMap, patternArgs))
			.filter((l): l is PatternLayer => l !== null);

		barLayersList.push(layers);
	}

	// Group consecutive bars with identical layers
	const groups: {
		startBar: number;
		endBar: number;
		layers: PatternLayer[];
	}[] = [];
	for (let i = 0; i < barLayersList.length; i++) {
		const layers = barLayersList[i];
		if (groups.length > 0) {
			const last = groups[groups.length - 1];
			if (layersEqual(last.layers, layers)) {
				last.endBar = i + 1;
				continue;
			}
		}
		groups.push({ startBar: i + 1, endBar: i + 1, layers });
	}

	// Build Document with hold detection
	const doc: Document = { bars: [] };
	let prevLayers: PatternLayer[] | null = null;

	for (const group of groups) {
		if (group.layers.length === 0) continue;

		const isHold = prevLayers !== null && layersEqual(prevLayers, group.layers);

		const block: BarBlock = {
			range: { start: group.startBar, end: group.endBar },
			layers: isHold ? [{ type: "hold", span: ZERO_SPAN }] : group.layers,
			span: ZERO_SPAN,
		};

		doc.bars.push(block);
		prevLayers = group.layers;
	}

	return serialize(doc, registry);
}

function annotationToLayer(
	annotation: TimelineAnnotation,
	patternNameMap: Map<number, string>,
	patternArgs: Record<number, BindingPatternArgDef[]>,
): PatternLayer | null {
	const patternName = patternNameMap.get(annotation.patternId);
	if (!patternName) return null;

	const argDefs = patternArgs[annotation.patternId] ?? [];
	const rawArgs = (annotation.args ?? {}) as Record<string, unknown>;

	// Extract selection expression from the Selection arg
	let selection: TagExpr = { type: "tag", name: "all" };
	for (const def of argDefs) {
		if (def.argType === "Selection") {
			const val = rawArgs[def.id];
			if (val && typeof val === "object" && "expression" in val) {
				const expr = (val as { expression: string }).expression;
				if (expr) {
					selection = parseTagExprString(expr);
				}
			}
			break;
		}
	}

	// Convert non-Selection args to DSL Arg format
	const args: Arg[] = [];
	for (const def of argDefs) {
		if (def.argType === "Selection") continue;
		const val = rawArgs[def.id];
		if (val == null) continue;

		const converted = convertArgValue(def.argType, val);
		if (!converted) continue;

		args.push({ key: def.name, value: converted, span: ZERO_SPAN });
	}

	return {
		type: "pattern",
		pattern: patternName,
		selection,
		args,
		blend: annotation.blendMode ?? DEFAULT_BLEND_MODE,
		span: ZERO_SPAN,
	};
}

function convertArgValue(argType: string, value: unknown): ArgValue | null {
	if (argType === "Color") {
		if (typeof value === "object" && value !== null && "r" in value) {
			const { r, g, b } = value as { r: number; g: number; b: number };
			return { type: "color", hex: rgbToHex(r, g, b) };
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

function rgbToHex(r: number, g: number, b: number): string {
	const rh = Math.round(Math.max(0, Math.min(255, r)))
		.toString(16)
		.padStart(2, "0");
	const gh = Math.round(Math.max(0, Math.min(255, g)))
		.toString(16)
		.padStart(2, "0");
	const bh = Math.round(Math.max(0, Math.min(255, b)))
		.toString(16)
		.padStart(2, "0");
	return `#${rh}${gh}${bh}`;
}

export function buildRegistry(
	patterns: PatternSummary[],
	patternArgs: Record<number, BindingPatternArgDef[]>,
): PatternRegistry {
	const defs: PatternDef[] = patterns.map((p) => {
		const args = (patternArgs[p.id] ?? []).map((a) => ({
			id: a.id,
			name: a.name,
			argType: a.argType,
			defaultValue: convertDefaultValue(a.argType, a.defaultValue),
		}));
		return { name: p.name, args };
	});
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
			return rgbToHex(r, g, b);
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

// ── Layer comparison for hold detection ──────────────────────────

function layersEqual(a: PatternLayer[], b: PatternLayer[]): boolean {
	if (a.length !== b.length) return false;
	for (let i = 0; i < a.length; i++) {
		if (!layerEqual(a[i], b[i])) return false;
	}
	return true;
}

function layerEqual(a: PatternLayer, b: PatternLayer): boolean {
	if (a.pattern !== b.pattern) return false;
	if (a.blend !== b.blend) return false;
	if (tagExprKey(a.selection) !== tagExprKey(b.selection)) return false;
	if (a.args.length !== b.args.length) return false;
	for (let i = 0; i < a.args.length; i++) {
		if (a.args[i].key !== b.args[i].key) return false;
		if (!argValueEqual(a.args[i].value, b.args[i].value)) return false;
	}
	return true;
}

function argValueEqual(a: ArgValue, b: ArgValue): boolean {
	if (a.type !== b.type) return false;
	switch (a.type) {
		case "color":
			return a.hex === (b as { type: "color"; hex: string }).hex;
		case "number":
			return a.value === (b as { type: "number"; value: number }).value;
		case "identifier":
			return a.value === (b as { type: "identifier"; value: string }).value;
	}
}

function tagExprKey(expr: TagExpr): string {
	switch (expr.type) {
		case "tag":
			return expr.name;
		case "not":
			return `~${tagExprKey(expr.operand)}`;
		case "and":
			return `(${tagExprKey(expr.left)}&${tagExprKey(expr.right)})`;
		case "or":
			return `(${tagExprKey(expr.left)}|${tagExprKey(expr.right)})`;
		case "xor":
			return `(${tagExprKey(expr.left)}^${tagExprKey(expr.right)})`;
		case "fallback":
			return `(${tagExprKey(expr.left)}>${tagExprKey(expr.right)})`;
		case "group":
			return tagExprKey(expr.inner);
	}
}

// ── DSL → Annotations (import) ───────────────────────────────────

export type DslAnnotation = {
	patternId: number;
	startTime: number;
	endTime: number;
	zIndex: number;
	blendMode: BlendMode;
	args: Record<string, unknown>;
};

export function dslToAnnotations(
	document: Document,
	beatGrid: BeatGrid,
	patterns: PatternSummary[],
	patternArgs: Record<number, BindingPatternArgDef[]>,
): DslAnnotation[] {
	if (document.bars.length === 0 || beatGrid.downbeats.length === 0) {
		return [];
	}

	const patternIdMap = new Map(patterns.map((p) => [p.name, p.id]));
	const totalBars = beatGrid.downbeats.length;

	function barStartTime(barNumber: number): number {
		const idx = barNumber - 1;
		if (idx < 0) return 0;
		if (idx < totalBars) return beatGrid.downbeats[idx];
		// Extrapolate past the last bar
		const lastBarStart = beatGrid.downbeats[totalBars - 1];
		const barDuration =
			totalBars >= 2
				? beatGrid.downbeats[totalBars - 1] - beatGrid.downbeats[totalBars - 2]
				: (60 / beatGrid.bpm) * beatGrid.beatsPerBar;
		return lastBarStart + (idx - (totalBars - 1)) * barDuration;
	}

	function barEndTime(barNumber: number): number {
		return barStartTime(barNumber + 1);
	}

	// Resolve holds: track current layers state
	let prevLayers: PatternLayer[] | null = null;
	const annotations: DslAnnotation[] = [];

	for (const block of document.bars) {
		const startTime = barStartTime(block.range.start);
		const endTime = barEndTime(block.range.end);

		// Determine effective layers (resolve holds)
		let effectiveLayers: PatternLayer[];
		if (
			block.layers.length === 1 &&
			block.layers[0].type === "hold" &&
			prevLayers !== null
		) {
			effectiveLayers = prevLayers;
		} else {
			effectiveLayers = block.layers.filter(
				(l): l is PatternLayer => l.type === "pattern",
			);
			prevLayers = effectiveLayers;
		}

		let zIndex = 0;
		for (const layer of effectiveLayers) {
			const patternId = patternIdMap.get(layer.pattern);
			if (patternId === undefined) continue;

			const argDefs = patternArgs[patternId] ?? [];
			const args = convertLayerArgs(layer, argDefs);

			annotations.push({
				patternId,
				startTime,
				endTime,
				zIndex,
				blendMode: layer.blend,
				args,
			});
			zIndex++;
		}
	}

	return annotations;
}

function convertLayerArgs(
	layer: PatternLayer,
	argDefs: BindingPatternArgDef[],
): Record<string, unknown> {
	const args: Record<string, unknown> = {};

	for (const def of argDefs) {
		if (def.argType === "Selection") {
			// Serialize tag expression back to string for the Selection arg
			const exprStr = serializeTagExpr(layer.selection);
			args[def.id] = { expression: exprStr, spatialReference: "global" };
			continue;
		}

		// Look for an explicit value in the DSL layer args
		const dslArg = layer.args.find((a) => a.key === def.name);
		if (dslArg) {
			args[def.id] = convertArgValueToAnnotation(dslArg.value, def.argType);
		} else if (def.defaultValue != null) {
			// Use the default value from the pattern definition
			args[def.id] = def.defaultValue;
		}
	}

	return args;
}

function convertArgValueToAnnotation(
	value: ArgValue,
	argType: string,
): unknown {
	if (value.type === "color" && argType === "Color") {
		return hexToRgb(value.hex);
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

export function hexToRgb(hex: string): { r: number; g: number; b: number } {
	const clean = hex.replace(/^#/, "");
	const r = Number.parseInt(clean.slice(0, 2), 16);
	const g = Number.parseInt(clean.slice(2, 4), 16);
	const b = Number.parseInt(clean.slice(4, 6), 16);
	return { r, g, b };
}

// ── Minimal tag expression parser ────────────────────────────────

export function parseTagExprString(input: string): TagExpr {
	let pos = 0;

	function skipWS() {
		while (pos < input.length && input[pos] === " ") pos++;
	}

	function parseFallback(): TagExpr {
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

	function parseOr(): TagExpr {
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

	function parseXor(): TagExpr {
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

	function parseAnd(): TagExpr {
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

	function parseUnary(): TagExpr {
		skipWS();
		if (pos < input.length && input[pos] === "~") {
			pos++;
			const operand = parseUnary();
			return { type: "not", operand };
		}
		return parsePrimary();
	}

	function parsePrimary(): TagExpr {
		skipWS();
		if (pos < input.length && input[pos] === "(") {
			pos++;
			const inner = parseFallback();
			skipWS();
			if (pos < input.length && input[pos] === ")") pos++;
			return { type: "group", inner };
		}
		let name = "";
		while (pos < input.length && /[a-zA-Z0-9_]/.test(input[pos])) {
			name += input[pos];
			pos++;
		}
		return { type: "tag", name: name || "all" };
	}

	return parseFallback();
}
