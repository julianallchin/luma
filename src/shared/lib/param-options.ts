import type { ParamDef, ParamOption } from "@/bindings/schema";

/**
 * The closed set of values a param accepts, or `null` when it is free-form.
 *
 * The options are projected from the compiler's own lowering tables
 * (`eval::ops::math::MATH_OPS` and friends), so a picker built on them cannot
 * offer a value that fails to compile. Never hand-author an option list
 * alongside one of these — extend the table instead.
 */
export function paramOptions(param: ParamDef): ParamOption[] | null {
	return typeof param.paramType === "object"
		? param.paramType.Enum.options
		: null;
}
