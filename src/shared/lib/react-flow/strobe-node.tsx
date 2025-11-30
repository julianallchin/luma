import * as React from "react";
import type { NodeProps } from "reactflow";
import { BaseNode } from "./base-node";
import type { BaseNodeData } from "./types";

// Strobe node
export function StrobeNode(props: NodeProps<BaseNodeData>) {
	const { data } = props;

	// No specific controls for now, just render the base node
	const controls = <div className="p-3">Strobe Node Controls (Coming Soon!)</div>;

	return <BaseNode {...props} data={{ ...data, paramControls: controls }} />;
}
