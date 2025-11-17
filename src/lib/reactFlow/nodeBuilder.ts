import type { Node } from "reactflow";
import type { NodeTypeDef, PortType } from "../../bindings/schema";
import type {
	BaseNodeData,
	HarmonyColorVisualizerNodeData,
	MelSpecNodeData,
	PatternEntryNodeData,
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
	port: { id: string; name: string; portType: PortType },
	direction: "in" | "out",
): PortDef {
	return {
		id: port.id,
		label: port.name,
		direction,
		portType: port.portType,
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
): Node<
	| BaseNodeData
	| ViewChannelNodeData
	| MelSpecNodeData
	| PatternEntryNodeData
	| HarmonyColorVisualizerNodeData
> {
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
		if (definition.id === "view_channel") return "viewChannel";
		if (definition.id === "audio_source") return "audioSource";
		if (definition.id === "mel_spec_viewer") return "melSpec";
		if (definition.id === "pattern_entry") return "patternEntry";
		if (definition.id === "color") return "color";
		if (definition.id === "harmony_color_visualizer")
			return "harmonyColorVisualizer";
		return "standard";
	})();
	const nodeId = `node-${++nodeIdCounter}`;

	if (nodeType === "viewChannel") {
		const viewData: ViewChannelNodeData = {
			...baseData,
			viewSamples: null,
			seriesData: null,
			playbackSourceId: null,
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
			playbackSourceId: null,
		};
		return {
			id: nodeId,
			type: nodeType,
			position: position ?? { x: 0, y: 0 },
			data: melData,
		};
	}

	if (nodeType === "patternEntry") {
		const entryData: PatternEntryNodeData = {
			...baseData,
			patternEntry: null,
		};
		return {
			id: nodeId,
			type: nodeType,
			position: position ?? { x: 0, y: 0 },
			data: entryData,
		};
	}

	if (nodeType === "harmonyColorVisualizer") {
		const harmonyData: HarmonyColorVisualizerNodeData = {
			...baseData,
			seriesData: null,
			baseColor: null,
			playbackSourceId: null,
		};
		return {
			id: nodeId,
			type: nodeType,
			position: position ?? { x: 0, y: 0 },
			data: harmonyData,
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
