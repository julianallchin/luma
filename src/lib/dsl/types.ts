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

// ── Group expressions ───────────────────────────────────────────────

export type GroupExpr =
	| { type: "group"; name: string }
	| { type: "not"; operand: GroupExpr }
	| { type: "and"; left: GroupExpr; right: GroupExpr }
	| { type: "or"; left: GroupExpr; right: GroupExpr }
	| { type: "xor"; left: GroupExpr; right: GroupExpr }
	| { type: "fallback"; left: GroupExpr; right: GroupExpr }
	| { type: "paren"; inner: GroupExpr };

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

// ── Bar range (half-open: [start, end) in fractional bar space) ────

export type BarRange = {
	start: number;
	end: number;
};

// ── Annotation (one pattern applied to a time range) ───────────────

export type Annotation = {
	type: "annotation";
	pattern: string;
	selection: GroupExpr;
	range: BarRange;
	args: Arg[];
	blend: BlendMode;
	span: Span;
};

// ── Document ───────────────────────────────────────────────────────
// Layers are groups of annotations at the same z-level.
// Layer 0 is the bottom (lowest priority), higher layers paint on top.
// Within a layer, annotations are in time order and should not overlap.

export type Document = {
	layers: Annotation[][];
};
