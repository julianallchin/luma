import type { Node } from "reactflow";
import type { NodeTypeDef, PortType } from "@/bindings/schema";
import type {
	BaseNodeData,
	MelSpecNodeData,
	PortDef,
	ViewChannelNodeData,
} from "./types";

// Node ID counter
let nodeIdCounter = 0;

/**
 * Ensure future node IDs don't collide with IDs that were loaded from storage.
 * This needs to run whenever we hydrate a saved graph so that creating a
 * new node doesn't reuse an existing ID (which ReactFlow treats as replacement).
 */
export function syncNodeIdCounter(existingNodeIds: string[]) {
	const maxId = existingNodeIds.reduce((max, id) => {
		const match = /^node-(\d+)$/.exec(id);
		if (!match) return max;
		const numericId = Number(match[1]);
		return Number.isNaN(numericId) ? max : Math.max(max, numericId);
	}, 0);
	if (maxId > nodeIdCounter) {
		nodeIdCounter = maxId;
	}
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
): Node<BaseNodeData | ViewChannelNodeData | MelSpecNodeData> {
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
		if (definition.id === "view_channel" || definition.id === "view_signal")
			return "viewChannel";
		if (definition.id === "audio_input") return "audioInput";
		if (definition.id === "beat_clock") return "beatClock";
		if (definition.id === "beat_envelope") return "beatEnvelope";
		if (definition.id === "mel_spec_viewer") return "melSpec";
		if (definition.id === "color") return "color";
		if (definition.id === "gradient") return "gradient";
		if (definition.id === "falloff") return "falloff";
		if (definition.id === "math") return "math";
		if (definition.id === "get_attribute") return "getAttribute";
		if (definition.id === "select") return "select";
		if (definition.id === "apply_strobe") return "standard";
		if (definition.id === "frequency_amplitude") return "frequencyAmplitude";
		if (definition.id === "threshold") return "threshold";
		if (definition.id === "invert") return "invert";
		return "standard";
	})();
	const nodeId = `node-${++nodeIdCounter}`;

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

	if (nodeType === "math") {
		return {
			id: nodeId,
			type: nodeType,
			position: position ?? { x: 0, y: 0 },
			data: baseData,
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
