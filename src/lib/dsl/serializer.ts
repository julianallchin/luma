import {
	type Annotation,
	type ArgValue,
	type BarRange,
	DEFAULT_BLEND_MODE,
	type Document,
	type GroupExpr,
	type PatternArgDef,
	type PatternRegistry,
} from "./types";

export type SerializeOptions = {
	/** Beats per bar for bar:beat:sub notation. Default: 4 */
	beatsPerBar?: number;
	/** Subdivisions per beat for bar:beat:sub notation. Default: 4 (sixteenth notes) */
	subsPerBeat?: number;
};

/** Format a finite number with enough precision to reconstruct the same f64. */
export function formatNumber(n: number): string {
	if (!Number.isFinite(n)) {
		throw new Error("Luma DSL cannot serialize a non-finite number");
	}
	return String(n);
}

export function serialize(
	doc: Document,
	registry: PatternRegistry,
	options?: SerializeOptions,
): string {
	const beatsPerBar = options?.beatsPerBar ?? 4;
	const subsPerBeat = options?.subsPerBeat ?? 4;
	const parts: string[] = [];
	const explicitZ =
		doc.zIndices?.some((zIndex, index) => zIndex !== index) ?? false;

	for (let i = 0; i < doc.layers.length; i++) {
		if (i > 0) parts.push("");
		if (explicitZ) {
			parts.push(`layer ${doc.zIndices?.[i] ?? i}:`);
		}
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
	if (range.unit === "seconds") {
		return `@${formatNumber(range.start)}s-${formatNumber(range.end)}s`;
	}

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

	if (annotation.id !== undefined) {
		parts.push(`${JSON.stringify(annotation.id)}:`);
	}

	const patternName = /^[a-zA-Z_][a-zA-Z0-9_]*$/.test(annotation.pattern)
		? annotation.pattern
		: JSON.stringify(annotation.pattern);
	const stablePatternRef =
		annotation.patternId === undefined
			? patternName
			: `${patternName}[${JSON.stringify(annotation.patternId)}]`;
	let selection = "";
	if (annotation.selection !== null) {
		const expression = serializeGroupExpr(annotation.selection);
		selection =
			annotation.selectionSpatialReference &&
			annotation.selectionSpatialReference !== "global"
				? `${annotation.selectionSpatialReference}: ${expression}`
				: expression;
	}
	parts.push(`${stablePatternRef}(${selection})`);

	// Bar range
	parts.push(serializeBarRange(annotation.range, beatsPerBar, subsPerBeat));

	// Args — emit every present arg exactly once. Stable IDs determine definition
	// order first; a display name is only an alias when it uniquely names one arg
	// and does not collide with any stable ID.
	const patternDef = findPatternDef(
		registry,
		annotation.pattern,
		annotation.patternId,
	);
	if (patternDef) {
		const orderedArgs = annotation.args
			.map((arg, sourceIndex) => ({
				arg,
				sourceIndex,
				definitionIndex: definitionIndexForArgKey(arg.key, patternDef.args),
			}))
			.sort(
				(left, right) =>
					left.definitionIndex - right.definitionIndex ||
					left.sourceIndex - right.sourceIndex,
			);
		for (const { arg } of orderedArgs) {
			parts.push(`${serializeArgKey(arg.key)}=${serializeArgValue(arg.value)}`);
		}
	} else {
		// No registry entry — emit all args in order
		for (const arg of annotation.args) {
			parts.push(`${serializeArgKey(arg.key)}=${serializeArgValue(arg.value)}`);
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
		case "json": {
			const serialized = JSON.stringify(value.value);
			if (serialized === undefined) {
				throw new Error("Luma DSL arguments must be JSON values");
			}
			return serialized;
		}
	}
}

function serializeArgKey(key: string): string {
	return /^[a-zA-Z_][a-zA-Z0-9_]*$/.test(key) && key !== "blend"
		? key
		: JSON.stringify(key);
}

function findPatternDef(registry: PatternRegistry, name: string, id?: string) {
	if (id === undefined) return registry.get(name);
	return [...new Set(registry.values())].find((def) => def.id === id);
}

function definitionIndexForArgKey(
	key: string,
	definitions: PatternArgDef[],
): number {
	const exactIndex = definitions.findIndex(
		(definition) => definition.id === key,
	);
	if (exactIndex >= 0) return exactIndex;

	const aliases = definitions
		.map((definition, index) => ({ definition, index }))
		.filter(({ definition }) => definition.name === key);
	if (
		aliases.length === 1 &&
		!definitions.some((definition) => definition.id === key)
	) {
		return aliases[0].index;
	}

	return definitions.length;
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
