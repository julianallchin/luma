import type { Connection, Node } from "reactflow";
import type {
	PortDef,
	BaseNodeData,
	ViewChannelNodeData,
	MelSpecNodeData,
} from "./types";

type AnyNodeData = BaseNodeData | ViewChannelNodeData | MelSpecNodeData;

function findPortInDefinition(
	node: Node<AnyNodeData>,
	handleId: string | null | undefined,
): PortDef | undefined {
	if (!handleId) return undefined;
	const definition = node.data.definition;
	if (!definition) return undefined;
	const input = definition.inputs.find((p) => p.id === handleId);
	if (input) {
		return {
			id: input.id,
			label: input.name,
			direction: "in" as const,
			portType: input.portType,
		};
	}
	const output = definition.outputs.find((p) => p.id === handleId);
	if (output) {
		return {
			id: output.id,
			label: output.name,
			direction: "out" as const,
			portType: output.portType,
		};
	}
	return undefined;
}

export function findPort(
	node: Node<AnyNodeData>,
	handleId: string | null | undefined,
): PortDef | undefined {
	if (!handleId) return undefined;
	return (
		findPortInDefinition(node, handleId) ??
		[...node.data.inputs, ...node.data.outputs].find(
			(p) => p.id === handleId,
		)
	);
}

function portTypesCompatible(a: PortDef, b: PortDef) {
	// Types must exactly match and directions must be opposite
	if (a.portType !== b.portType) return false;
	return a.direction !== b.direction;
}

export function makeIsValidConnection(
	nodes: Node<AnyNodeData>[],
): (connection: Connection) => boolean {
	return (connection: Connection) => {
		const sourceNode = nodes.find((n) => n.id === connection.source);
		const targetNode = nodes.find((n) => n.id === connection.target);

		if (!sourceNode || !targetNode) return false;

		const sourcePort = findPort(sourceNode, connection.sourceHandle);
		const targetPort = findPort(targetNode, connection.targetHandle);

		if (!sourcePort || !targetPort) return false;

		return portTypesCompatible(sourcePort, targetPort);
	};
}
