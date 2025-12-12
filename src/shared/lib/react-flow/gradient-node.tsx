import * as React from "react";
import type { NodeProps } from "reactflow";

import { useGraphStore } from "@/features/patterns/stores/use-graph-store";
import {
	GradientPicker,
	type GradientStop,
} from "@/shared/components/gradient-picker";
import { BaseNode } from "./base-node";
import type { BaseNodeData } from "./types";

export function GradientNode(props: NodeProps<BaseNodeData>) {
	const { data, id } = props;
	const params = useGraphStore(
		(state) => state.nodeParams[id] ?? ({} as Record<string, unknown>),
	);
	const setParam = useGraphStore((state) => state.setParam);

	const stopsParam = (params.stops as string) ?? "[]";
	let stops: GradientStop[] = [];
	try {
		stops = JSON.parse(stopsParam);
		if (!Array.isArray(stops)) stops = [];
	} catch {
		stops = [];
	}

	const handleGradientChange = React.useCallback(
		(newStops: GradientStop[]) => {
			setParam(id, "stops", JSON.stringify(newStops));
		},
		[id, setParam],
	);

	return (
		<BaseNode
			{...props}
			data={{
				...data,
				paramControls: (
					<GradientPicker
						value={stops}
						onChange={handleGradientChange}
						className="p-2 w-72 nodrag"
					/>
				),
			}}
		/>
	);
}
