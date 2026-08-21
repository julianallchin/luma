import type { NodeTypeDef, PatternArgDef, PortType } from "@/bindings/schema";

/**
 * The `pattern_args` node is synthetic: it has no entry in the backend node
 * catalogue, because its ports *are* the pattern's argument list. Anything that
 * hydrates a graph into the editor (the pattern editor, the capture harness)
 * has to synthesize the same definition, so it lives here rather than being
 * rebuilt per call site.
 */
export const PATTERN_ARGS_NODE_ID = "pattern_args";

/** Port type an argument surfaces on. Palettes and gradients are both `Stops`. */
function argPortType(argType: PatternArgDef["argType"]): PortType {
	if (argType === "Selection") return "Selection";
	if (argType === "Palette" || argType === "Gradient") return "Stops";
	return "Signal";
}

/** `null` when the pattern has no arguments — the node isn't rendered at all. */
export function patternArgsNodeDef(args: PatternArgDef[]): NodeTypeDef | null {
	if (args.length === 0) return null;
	return {
		id: PATTERN_ARGS_NODE_ID,
		name: "Pattern Args",
		description: "Arguments provided by track annotations.",
		category: "Input",
		inputs: [],
		outputs: args.map((arg) => ({
			id: arg.id,
			name: arg.name,
			portType: argPortType(arg.argType),
		})),
		params: [],
	};
}
