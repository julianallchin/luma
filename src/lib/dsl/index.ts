export type { DslAnnotation } from "./convert";
export { annotationsToDsl, dslToAnnotations } from "./convert";
export type { DslError, DslErrorCode, DslWarning } from "./errors";
export { formatError } from "./errors";
export type { ParseResult } from "./parser";
export { parse } from "./parser";
export { serialize, serializeTagExpr } from "./serializer";
export type {
	Arg,
	ArgValue,
	BarBlock,
	BarRange,
	BlendMode,
	Document,
	HoldLayer,
	Layer,
	Loc,
	PatternArgDef,
	PatternArgType,
	PatternDef,
	PatternLayer,
	PatternRegistry,
	Span,
	TagExpr,
} from "./types";
export { BLEND_MODES, DEFAULT_BLEND_MODE } from "./types";

import type { PatternDef, PatternRegistry } from "./types";

export function createRegistry(patterns: PatternDef[]): PatternRegistry {
	return new Map(patterns.map((p) => [p.name, p]));
}
