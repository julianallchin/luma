import type { Node } from "reactflow";
import type { NodeTypeDef, PortType } from "@/bindings/schema";
import type {
	BaseNodeData,
	MelSpecNodeData,
	PortDef,
	UvViewNodeData,
	ViewChannelNodeData,
} from "./types";

// Per-type-prefix high-water counters. Node ids are human-readable and double
// as the canonical handle used everywhere — the saved graph JSON, ReactFlow,
// backend compile errors, and the graph agent — so there's no separate "alias"
// namespace. New ids look like `apply_color_1`, `apply_color_2`, … Legacy
// `node-7` ids (dash-separated) still load fine as opaque strings; they just
// don't participate in the typed counters.
const nodeIdCounters = new Map<string, number>();

/** Parse a typed id like `apply_color_3` into its `{prefix, n}`. Returns null
 * for ids that don't follow the scheme (e.g. legacy `node-7`). */
function parseTypedId(id: string): { prefix: string; n: number } | null {
	const match = /^(.+)_(\d+)$/.exec(id);
	if (!match) return null;
	const n = Number(match[2]);
	if (Number.isNaN(n)) return null;
	return { prefix: match[1], n };
}

/**
 * Ensure future node IDs don't collide with IDs that were loaded from storage.
 * This needs to run whenever we hydrate a saved graph so that creating a
 * new node doesn't reuse an existing ID (which ReactFlow treats as replacement).
 */
export function syncNodeIdCounter(existingNodeIds: string[]) {
	for (const id of existingNodeIds) {
		const parsed = parseTypedId(id);
		if (!parsed) continue;
		const current = nodeIdCounters.get(parsed.prefix) ?? 0;
		if (parsed.n > current) nodeIdCounters.set(parsed.prefix, parsed.n);
	}
}

/** Next unique, human-readable id for a node of the given type. */
export function nextNodeId(typeId: string): string {
	const next = (nodeIdCounters.get(typeId) ?? 0) + 1;
	nodeIdCounters.set(typeId, next);
	return `${typeId}_${next}`;
}

// Convert PortType to PortDef
function convertPortDef(
	port: { id: string; name: string; portType?: PortType; port_type?: PortType },
	direction: "in" | "out",
): PortDef {
	// Be defensive about casing from the backend (portType vs port_type)
	const portType = port.portType ?? port.port_type;
	return {
		id: port.id,
		label: port.name,
		direction,
		portType: (portType ?? "Signal") as PortType,
	};
}

// Serialize params
function serializeParams(params: Record<string, unknown>) {
	return Object.keys(params).reduce<Record<string, unknown>>((acc, key) => {
		const value = params[key];
		if (value !== undefined) {
			acc[key] = value;
		}
		return acc;
	}, {});
}

// Convert NodeTypeDef to ReactFlow node
export function buildNode(
	definition: NodeTypeDef,
	onChange: () => void,
	position?: { x: number; y: number },
): Node<BaseNodeData | ViewChannelNodeData | UvViewNodeData | MelSpecNodeData> {
	const inputs = definition.inputs.map((p) => convertPortDef(p, "in"));
	const outputs = definition.outputs.map((p) => convertPortDef(p, "out"));

	const baseData: BaseNodeData = {
		title: definition.name,
		inputs,
		outputs,
		typeId: definition.id,
		definition,
		params: {},
		onChange,
	};

	// Apply parameter defaults
	for (const param of definition.params) {
		if (param.paramType === "Number") {
			baseData.params[param.id] = param.defaultNumber ?? 0;
		} else if (param.paramType === "Text") {
			baseData.params[param.id] = param.defaultText ?? "";
		}
	}

	const nodeType = (() => {
		if (
			definition.id === "view_channel" ||
			definition.id === "view_signal" ||
			definition.id === "view_events"
		)
			return "viewChannel";
		if (definition.id === "view_uv") return "uvView";
		if (definition.id === "audio_input") return "audioInput";
		if (definition.id === "beat_envelope") return "beatEnvelope";
		if (definition.id === "adsr") return "adsr";
		if (definition.id === "mel_spec_viewer") return "melSpec";
		if (definition.id === "color") return "color";
		if (definition.id === "palette") return "palette";
		if (definition.id === "gradient") return "gradient";
		if (definition.id === "falloff") return "falloff";
		if (definition.id === "noise") return "noise";
		if (definition.id === "rainbow") return "rainbow";
		if (definition.id === "filter_selection") return "filterSelection";
		if (definition.id === "apply_strobe") return "standard";
		if (definition.id === "frequency_amplitude") return "frequencyAmplitude";
		if (definition.id === "threshold") return "threshold";
		if (definition.id === "invert") return "invert";
		return "standard";
	})();
	const nodeId = nextNodeId(definition.id);

	if (nodeType === "viewChannel") {
		const viewData: ViewChannelNodeData = {
			...baseData,
			viewSamples: null,
		};
		return {
			id: nodeId,
			type: nodeType,
			position: position ?? { x: 0, y: 0 },
			data: viewData,
		};
	}

	if (nodeType === "uvView") {
		const uvData: UvViewNodeData = {
			...baseData,
			viewSamples: null,
		};
		return {
			id: nodeId,
			type: nodeType,
			position: position ?? { x: 0, y: 0 },
			data: uvData,
		};
	}

	if (nodeType === "melSpec") {
		const melData: MelSpecNodeData = {
			...baseData,
			melSpec: undefined,
		};
		return {
			id: nodeId,
			type: nodeType,
			position: position ?? { x: 0, y: 0 },
			data: melData,
		};
	}

	return {
		id: nodeId,
		type: nodeType,
		position: position ?? { x: 0, y: 0 },
		data: baseData,
	};
}

export { serializeParams };
