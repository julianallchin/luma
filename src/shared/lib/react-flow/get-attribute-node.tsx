import type * as React from "react";
import type { NodeProps } from "reactflow";
import { useGraphStore } from "@/features/patterns/stores/use-graph-store";
import { Selector } from "@/shared/components/ui/selector";
import { BaseNode } from "./base-node";
import type { BaseNodeData } from "./types";

const ATTRIBUTE_OPTIONS = [
	{ label: "Index", value: "index" },
	{ label: "Normalized Index", value: "normalized_index" },
	{ label: "Count", value: "count" },
	{ label: "Position X", value: "pos_x" },
	{ label: "Position Y", value: "pos_y" },
	{ label: "Position Z", value: "pos_z" },
	{ label: "Relative X", value: "rel_x" },
	{ label: "Relative Y", value: "rel_y" },
	{ label: "Relative Z", value: "rel_z" },
	{ label: "U (rig axis)", value: "u" },
	{ label: "V (height)", value: "v" },
	{ label: "Major Span", value: "rel_major_span" },
	{ label: "Major Count", value: "rel_major_count" },
	{ label: "Angular Position", value: "angular_position" },
	{ label: "Angular Index", value: "angular_index" },
	{ label: "Circle Radius", value: "circle_radius" },
	{ label: "Local X", value: "local_x" },
	{ label: "Local Y", value: "local_y" },
	{ label: "Local Z", value: "local_z" },
];

export function GetAttributeNode(props: NodeProps<BaseNodeData>) {
	const { data, id } = props;
	const params = useGraphStore(
		(state) => state.nodeParams[id] ?? ({} as Record<string, unknown>),
	);
	const setParam = useGraphStore((state) => state.setParam);

	const controls: React.ReactNode[] = [];
	for (const param of data.definition.params) {
		if (param.id === "attribute") {
			const value =
				(params[param.id] as string) ?? param.defaultText ?? "index";

			controls.push(
				<div key={param.id} className="px-2 pb-1">
					<span className="block text-[10px] text-gray-400 mb-1">
						{param.name}
					</span>
					<Selector
						value={value}
						onChange={(newValue) => setParam(id, param.id, newValue)}
						align="start"
						placeholder="Select attribute"
						options={ATTRIBUTE_OPTIONS}
					/>
				</div>,
			);
		} else {
			// Fallback for other potential params (though currently there are none)
			controls.push(
				<div key={param.id} className="px-2 pb-1 text-xs text-muted-foreground">
					{param.name} (Not implemented)
				</div>,
			);
		}
	}

	const paramControls =
		controls.length > 0 ? <div className="py-1">{controls}</div> : null;

	return <BaseNode {...props} data={{ ...data, paramControls }} />;
}
