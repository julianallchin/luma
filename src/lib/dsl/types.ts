// ── Pattern Registry (provided to parser) ──────────────────────────

export type PatternArgType = "Color" | "Scalar" | "Selection";

export type PatternArgDef = {
	id: string;
	name: string;
	argType: PatternArgType;
	defaultValue: unknown;
};

export type PatternDef = {
	name: string;
	args: PatternArgDef[];
};

export type PatternRegistry = ReadonlyMap<string, PatternDef>;

// ── Source locations ────────────────────────────────────────────────

export type Loc = {
	line: number;
	column: number;
	offset: number;
};

export type Span = {
	start: Loc;
	end: Loc;
};

// ── Blend modes ────────────────────────────────────────────────────

export const BLEND_MODES = [
	"replace",
	"add",
	"multiply",
	"screen",
	"max",
	"min",
	"lighten",
	"value",
	"subtract",
] as const;

export type BlendMode = (typeof BLEND_MODES)[number];

export const DEFAULT_BLEND_MODE: BlendMode = "replace";

// ── Tag expressions ────────────────────────────────────────────────

export type TagExpr =
	| { type: "tag"; name: string }
	| { type: "not"; operand: TagExpr }
	| { type: "and"; left: TagExpr; right: TagExpr }
	| { type: "or"; left: TagExpr; right: TagExpr }
	| { type: "xor"; left: TagExpr; right: TagExpr }
	| { type: "fallback"; left: TagExpr; right: TagExpr }
	| { type: "group"; inner: TagExpr };

// ── Arg values ─────────────────────────────────────────────────────

export type ArgValue =
	| { type: "color"; hex: string }
	| { type: "number"; value: number }
	| { type: "identifier"; value: string };

export type Arg = {
	key: string;
	value: ArgValue;
	span: Span;
};

// ── Layers ─────────────────────────────────────────────────────────

export type PatternLayer = {
	type: "pattern";
	pattern: string;
	selection: TagExpr;
	args: Arg[];
	blend: BlendMode;
	span: Span;
};

export type HoldLayer = {
	type: "hold";
	span: Span;
};

export type Layer = PatternLayer | HoldLayer;

// ── Bar blocks ─────────────────────────────────────────────────────

export type BarRange = {
	start: number;
	end: number;
};

export type BarBlock = {
	range: BarRange;
	layers: Layer[];
	span: Span;
};

// ── Document ───────────────────────────────────────────────────────

export type Document = {
	bars: BarBlock[];
};
