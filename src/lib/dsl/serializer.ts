import {
	type ArgValue,
	type BarBlock,
	DEFAULT_BLEND_MODE,
	type Document,
	type Layer,
	type PatternLayer,
	type PatternRegistry,
	type TagExpr,
} from "./types";

export function serialize(doc: Document, registry: PatternRegistry): string {
	const parts: string[] = [];

	for (let i = 0; i < doc.bars.length; i++) {
		if (i > 0) parts.push("");
		parts.push(serializeBarBlock(doc.bars[i], registry));
	}

	return parts.join("\n");
}

function serializeBarBlock(block: BarBlock, registry: PatternRegistry): string {
	const lines: string[] = [];

	// Bar header
	if (block.range.start === block.range.end) {
		lines.push(`@${block.range.start}`);
	} else {
		lines.push(`@${block.range.start}-${block.range.end}`);
	}

	// Layers
	for (const layer of block.layers) {
		lines.push(serializeLayer(layer, registry));
	}

	return lines.join("\n");
}

function serializeLayer(layer: Layer, registry: PatternRegistry): string {
	if (layer.type === "hold") {
		return "hold";
	}

	return serializePatternLayer(layer, registry);
}

function serializePatternLayer(
	layer: PatternLayer,
	registry: PatternRegistry,
): string {
	const parts: string[] = [];

	// Pattern name + selection
	parts.push(`${layer.pattern}(${serializeTagExpr(layer.selection)})`);

	// Args in definition order, skipping defaults
	const patternDef = registry.get(layer.pattern);
	if (patternDef) {
		for (const argDef of patternDef.args) {
			if (argDef.argType === "Selection") continue;

			const arg = layer.args.find((a) => a.key === argDef.name);
			if (!arg) continue;

			// Skip if value equals default
			if (isDefaultValue(arg.value, argDef.defaultValue)) continue;

			parts.push(`${arg.key}=${serializeArgValue(arg.value)}`);
		}

		// Also emit any args not in the definition (unknown args preserved)
		for (const arg of layer.args) {
			const inDef = patternDef.args.find(
				(d) => d.name === arg.key && d.argType !== "Selection",
			);
			if (!inDef) {
				parts.push(`${arg.key}=${serializeArgValue(arg.value)}`);
			}
		}
	} else {
		// No registry entry — emit all args in order
		for (const arg of layer.args) {
			parts.push(`${arg.key}=${serializeArgValue(arg.value)}`);
		}
	}

	// Blend mode (only if non-default)
	if (layer.blend !== DEFAULT_BLEND_MODE) {
		parts.push(`blend=${layer.blend}`);
	}

	return parts.join(" ");
}

function isDefaultValue(value: ArgValue, defaultValue: unknown): boolean {
	if (defaultValue == null) return false;

	switch (value.type) {
		case "color":
			return (
				typeof defaultValue === "string" &&
				value.hex.toLowerCase() === defaultValue.toLowerCase()
			);
		case "number":
			return typeof defaultValue === "number" && value.value === defaultValue;
		case "identifier":
			return typeof defaultValue === "string" && value.value === defaultValue;
	}
}

function serializeArgValue(value: ArgValue): string {
	switch (value.type) {
		case "color":
			return value.hex;
		case "number":
			return String(value.value);
		case "identifier":
			return value.value;
	}
}

export function serializeTagExpr(expr: TagExpr): string {
	switch (expr.type) {
		case "tag":
			return expr.name;
		case "not":
			return `~${serializeTagExpr(expr.operand)}`;
		case "and":
			return `${serializeTagExprPrec(expr.left, "and")} & ${serializeTagExprPrec(expr.right, "and")}`;
		case "or":
			return `${serializeTagExprPrec(expr.left, "or")} | ${serializeTagExprPrec(expr.right, "or")}`;
		case "xor":
			return `${serializeTagExprPrec(expr.left, "xor")} ^ ${serializeTagExprPrec(expr.right, "xor")}`;
		case "fallback":
			return `${serializeTagExprPrec(expr.left, "fallback")} > ${serializeTagExprPrec(expr.right, "fallback")}`;
		case "group":
			return `(${serializeTagExpr(expr.inner)})`;
	}
}

// Precedence levels (higher = tighter binding)
const PREC: Record<string, number> = {
	fallback: 1,
	or: 2,
	xor: 3,
	and: 4,
	not: 5,
	tag: 6,
	group: 6,
};

function serializeTagExprPrec(expr: TagExpr, parentOp: string): string {
	const exprPrec = PREC[expr.type] ?? 0;
	const parentPrec = PREC[parentOp] ?? 0;

	if (exprPrec < parentPrec) {
		return `(${serializeTagExpr(expr)})`;
	}
	return serializeTagExpr(expr);
}
