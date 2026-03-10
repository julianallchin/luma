import {
	type Annotation,
	type ArgValue,
	type BarRange,
	DEFAULT_BLEND_MODE,
	type Document,
	type GroupExpr,
	type PatternRegistry,
} from "./types";

export type SerializeOptions = {
	/** Beats per bar for bar:beat:sub notation. Default: 4 */
	beatsPerBar?: number;
	/** Subdivisions per beat for bar:beat:sub notation. Default: 4 (sixteenth notes) */
	subsPerBeat?: number;
};

/**
 * Format a number cleanly: round to 4 decimal places, strip trailing zeros.
 */
export function formatNumber(n: number): string {
	const rounded = Math.round(n * 10000) / 10000;
	return String(rounded);
}

export function serialize(
	doc: Document,
	registry: PatternRegistry,
	options?: SerializeOptions,
): string {
	const beatsPerBar = options?.beatsPerBar ?? 4;
	const subsPerBeat = options?.subsPerBeat ?? 4;
	const parts: string[] = [];

	for (let i = 0; i < doc.layers.length; i++) {
		if (i > 0) parts.push("");
		for (const annotation of doc.layers[i]) {
			parts.push(
				serializeAnnotation(annotation, registry, beatsPerBar, subsPerBeat),
			);
		}
	}

	return parts.join("\n");
}

/**
 * Convert a fractional bar number to bar:beat:sub notation.
 *
 * Rules:
 * - Whole bar → just the bar number: "5"
 * - On a beat boundary → bar:beat: "5:3"
 * - On a subdivision boundary → bar:beat:sub: "5:3:2"
 */
function formatBarPosition(
	fractional: number,
	beatsPerBar: number,
	subsPerBeat: number,
): string {
	const bar = Math.floor(fractional);
	const remainder = fractional - bar;

	// Check if it's a whole bar
	if (Math.abs(remainder) < 1e-9) {
		return String(bar);
	}

	// Convert to beat + sub
	const totalSubs = beatsPerBar * subsPerBeat;
	const subIndex = Math.round(remainder * totalSubs);

	const beat = Math.floor(subIndex / subsPerBeat) + 1; // 1-indexed
	const sub = (subIndex % subsPerBeat) + 1; // 1-indexed

	if (sub === 1) {
		// Exactly on a beat boundary
		return `${bar}:${beat}`;
	}

	return `${bar}:${beat}:${sub}`;
}

function serializeBarRange(
	range: BarRange,
	beatsPerBar: number,
	subsPerBeat: number,
): string {
	const startStr = formatBarPosition(range.start, beatsPerBar, subsPerBeat);
	const endStr = formatBarPosition(range.end, beatsPerBar, subsPerBeat);

	// Single bar shorthand: @5 when range is [5, 6) and start is a whole bar
	if (range.end === range.start + 1 && Number.isInteger(range.start)) {
		return `@${startStr}`;
	}

	return `@${startStr}-${endStr}`;
}

function serializeAnnotation(
	annotation: Annotation,
	registry: PatternRegistry,
	beatsPerBar: number,
	subsPerBeat: number,
): string {
	const parts: string[] = [];

	// Pattern name + selection
	parts.push(
		`${annotation.pattern}(${serializeGroupExpr(annotation.selection)})`,
	);

	// Bar range
	parts.push(serializeBarRange(annotation.range, beatsPerBar, subsPerBeat));

	// Args — emit all present args in definition order, then any unknown args
	const patternDef = registry.get(annotation.pattern);
	if (patternDef) {
		for (const argDef of patternDef.args) {
			if (argDef.argType === "Selection") continue;

			const arg = annotation.args.find((a) => a.key === argDef.name);
			if (!arg) continue;

			parts.push(`${arg.key}=${serializeArgValue(arg.value)}`);
		}

		// Also emit any args not in the definition (unknown args preserved)
		for (const arg of annotation.args) {
			const inDef = patternDef.args.find(
				(d) => d.name === arg.key && d.argType !== "Selection",
			);
			if (!inDef) {
				parts.push(`${arg.key}=${serializeArgValue(arg.value)}`);
			}
		}
	} else {
		// No registry entry — emit all args in order
		for (const arg of annotation.args) {
			parts.push(`${arg.key}=${serializeArgValue(arg.value)}`);
		}
	}

	// Blend mode (only if non-default)
	if (annotation.blend !== DEFAULT_BLEND_MODE) {
		parts.push(`blend=${annotation.blend}`);
	}

	return parts.join(" ");
}

function serializeArgValue(value: ArgValue): string {
	switch (value.type) {
		case "color":
			return value.hex;
		case "number":
			return formatNumber(value.value);
		case "identifier":
			return value.value;
	}
}

export function serializeGroupExpr(expr: GroupExpr): string {
	switch (expr.type) {
		case "group":
			return expr.name;
		case "not":
			return `~${serializeGroupExpr(expr.operand)}`;
		case "and":
			return `${serializeGroupExprPrec(expr.left, "and")} & ${serializeGroupExprPrec(expr.right, "and")}`;
		case "or":
			return `${serializeGroupExprPrec(expr.left, "or")} | ${serializeGroupExprPrec(expr.right, "or")}`;
		case "xor":
			return `${serializeGroupExprPrec(expr.left, "xor")} ^ ${serializeGroupExprPrec(expr.right, "xor")}`;
		case "fallback":
			return `${serializeGroupExprPrec(expr.left, "fallback")} > ${serializeGroupExprPrec(expr.right, "fallback")}`;
		case "paren":
			return `(${serializeGroupExpr(expr.inner)})`;
	}
}

// Precedence levels (higher = tighter binding)
const PREC: Record<string, number> = {
	fallback: 1,
	or: 2,
	xor: 3,
	and: 4,
	not: 5,
	group: 6,
	paren: 6,
};

function serializeGroupExprPrec(expr: GroupExpr, parentOp: string): string {
	const exprPrec = PREC[expr.type] ?? 0;
	const parentPrec = PREC[parentOp] ?? 0;

	if (exprPrec < parentPrec) {
		return `(${serializeGroupExpr(expr)})`;
	}
	return serializeGroupExpr(expr);
}
