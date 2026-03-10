export type { DslAnnotation } from "./convert";
export { annotationsToDsl, dslToAnnotations } from "./convert";
export type { DslError, DslErrorCode, DslWarning } from "./errors";
export { formatError } from "./errors";
export type { ParseOptions, ParseResult } from "./parser";
export { parse } from "./parser";
export type { SerializeOptions } from "./serializer";
export { serialize, serializeGroupExpr } from "./serializer";
export type {
	Annotation,
	Arg,
	ArgValue,
	BarRange,
	BlendMode,
	Document,
	GroupExpr,
	Loc,
	PatternArgDef,
	PatternArgType,
	PatternDef,
	PatternRegistry,
	Span,
} from "./types";
export { BLEND_MODES, DEFAULT_BLEND_MODE } from "./types";

import type { PatternDef, PatternRegistry } from "./types";

export function createRegistry(patterns: PatternDef[]): PatternRegistry {
	return new Map(patterns.map((p) => [p.name, p]));
}
