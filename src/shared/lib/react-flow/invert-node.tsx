import type { NodeProps } from "reactflow";
import { BaseNode } from "./base-node";
import type { BaseNodeData } from "./types";

export function InvertNode(props: NodeProps<BaseNodeData>) {
	return <BaseNode {...props} />;
}
