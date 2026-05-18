import * as React from "react";
import type { NodeProps } from "reactflow";
import { useGraphStore } from "@/features/patterns/stores/use-graph-store";
import {
	GradientStops,
	type GradientValue,
	parseGradientJson,
} from "@/shared/components/palette-editor";
import { BaseNode } from "./base-node";
import type { BaseNodeData } from "./types";

export function GradientNode(props: NodeProps<BaseNodeData>) {
	const { data, id } = props;
	const nodeParams = useGraphStore((state) => state.nodeParams);
	const params = nodeParams[id] ?? ({} as Record<string, unknown>);
	const setParam = useGraphStore((state) => state.setParam);

	const valueText = (params.value as string) ?? "";
	const value: GradientValue = React.useMemo(
		() => parseGradientJson(valueText),
		[valueText],
	);

	const updateValue = React.useCallback(
		(next: GradientValue) => {
			setParam(id, "value", JSON.stringify(next));
		},
		[id, setParam],
	);

	const controls = (
		<div className="px-3 pb-2 w-[280px]">
			<GradientStops value={value} onChange={updateValue} />
		</div>
	);

	return <BaseNode {...props} data={{ ...data, paramControls: controls }} />;
}
