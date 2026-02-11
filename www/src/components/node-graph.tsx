"use client";

import {
	type Edge,
	Handle,
	type Node,
	Position,
	ReactFlow,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { useMemo } from "react";

// Category → color mapping matching the app's node palette
const categoryColors: Record<string, string> = {
	input: "#6366f1", // indigo
	audio: "#8b5cf6", // violet
	generator: "#f59e0b", // amber
	selection: "#10b981", // emerald
	transform: "#64748b", // slate
	color: "#ec4899", // pink
	analysis: "#06b6d4", // cyan
	movement: "#f97316", // orange
	output: "#ef4444", // red
	view: "#a3a3a3", // neutral
};

function LumaNode({ data }: { data: Record<string, unknown> }) {
	const color = categoryColors[(data.category as string) || ""] || "#6366f1";
	const inputs = (data.inputs as string[]) || [];
	const outputs = (data.outputs as string[]) || [];
	const params = (data.params as Record<string, string>) || {};
	const paramEntries = Object.entries(params);

	return (
		<div
			style={{
				background: "#141414",
				border: `1px solid ${color}40`,
				borderRadius: 0,
				minWidth: 160,
				fontSize: 12,
				fontFamily: 'ui-monospace, SFMono-Regular, "SF Mono", Menlo, monospace',
				color: "#e5e5e5",
				overflow: "hidden",
			}}
		>
			{/* Header */}
			<div
				style={{
					background: `${color}20`,
					borderBottom: `1px solid ${color}40`,
					padding: "6px 12px",
					fontWeight: 600,
					fontSize: 11,
					textTransform: "uppercase",
					letterSpacing: "0.05em",
					color,
				}}
			>
				{data.label as string}
			</div>

			{/* Body */}
			<div style={{ padding: "6px 0", position: "relative" }}>
				{/* Input handles */}
				{inputs.map((name) => (
					<div
						key={`in-${name}`}
						style={{
							padding: "2px 12px",
							color: "#a3a3a3",
							fontSize: 10,
							position: "relative",
						}}
					>
						<Handle
							type="target"
							position={Position.Left}
							id={name}
							style={{
								background: color,
								width: 8,
								height: 8,
								border: "2px solid #141414",
								top: "50%",
							}}
						/>
						{name}
					</div>
				))}

				{/* Parameters */}
				{paramEntries.map(([key, val]) => (
					<div
						key={key}
						style={{
							padding: "2px 12px",
							display: "flex",
							justifyContent: "space-between",
							gap: 12,
						}}
					>
						<span style={{ color: "#737373", fontSize: 10 }}>{key}</span>
						<span style={{ color: "#e5e5e5", fontSize: 10 }}>{val}</span>
					</div>
				))}

				{/* Output handles */}
				{outputs.map((name) => (
					<div
						key={`out-${name}`}
						style={{
							padding: "2px 12px",
							color: "#a3a3a3",
							fontSize: 10,
							textAlign: "right",
							position: "relative",
						}}
					>
						{name}
						<Handle
							type="source"
							position={Position.Right}
							id={name}
							style={{
								background: color,
								width: 8,
								height: 8,
								border: "2px solid #141414",
								top: "50%",
							}}
						/>
					</div>
				))}

				{/* If no ports/params, add spacing */}
				{inputs.length === 0 &&
					outputs.length === 0 &&
					paramEntries.length === 0 && <div style={{ height: 4 }} />}
			</div>
		</div>
	);
}

const nodeTypes = { luma: LumaNode };

export interface NodeDef {
	id: string;
	label: string;
	category?: string;
	x: number;
	y: number;
	inputs?: string[];
	outputs?: string[];
	params?: Record<string, string>;
}

export interface EdgeDef {
	from: string;
	fromHandle?: string;
	to: string;
	toHandle?: string;
}

export function NodeGraph({
	nodes: nodeDefs,
	edges: edgeDefs,
	height = 400,
}: {
	nodes: NodeDef[];
	edges: EdgeDef[];
	height?: number;
}) {
	const nodes: Node[] = useMemo(
		() =>
			nodeDefs.map((n) => ({
				id: n.id,
				type: "luma",
				position: { x: n.x, y: n.y },
				data: {
					label: n.label,
					category: n.category || "",
					inputs: n.inputs || [],
					outputs: n.outputs || [],
					params: n.params || {},
				},
				draggable: false,
				selectable: false,
				connectable: false,
			})),
		[nodeDefs],
	);

	const edges: Edge[] = useMemo(
		() =>
			edgeDefs.map((e, i) => ({
				id: `e${i}`,
				source: e.from,
				sourceHandle: e.fromHandle,
				target: e.to,
				targetHandle: e.toHandle,
				style: { stroke: "#525252", strokeWidth: 2 },
				animated: true,
			})),
		[edgeDefs],
	);

	return (
		<div
			style={{
				width: "100%",
				height,
				borderRadius: 0,
				overflow: "hidden",
				border: "1px solid rgba(255,255,255,0.1)",
				background: "#0a0a0a",
				margin: "16px 0",
			}}
		>
			<ReactFlow
				nodes={nodes}
				edges={edges}
				nodeTypes={nodeTypes}
				fitView
				fitViewOptions={{ padding: 0.2, minZoom: 0.65, maxZoom: 1 }}
				minZoom={0.65}
				maxZoom={1}
				proOptions={{ hideAttribution: true }}
				panOnDrag
				zoomOnScroll={false}
				zoomOnPinch={false}
				zoomOnDoubleClick={false}
				nodesDraggable={false}
				nodesConnectable={false}
				elementsSelectable={false}
				preventScrolling={false}
			/>
		</div>
	);
}
