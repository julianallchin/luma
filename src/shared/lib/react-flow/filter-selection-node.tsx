import type { NodeProps } from "reactflow";
import { useGraphStore } from "@/features/patterns/stores/use-graph-store";
import { Selector } from "@/shared/components/ui/selector";
import { BaseNode } from "./base-node";
import type { BaseNodeData } from "./types";

const CAPABILITY_OPTIONS = [
	{ label: "Movement", value: "movement" },
	{ label: "Color", value: "color" },
	{ label: "Strobe", value: "strobe" },
];

export function FilterSelectionNode(props: NodeProps<BaseNodeData>) {
	const { data, id } = props;
	const params = useGraphStore(
		(state) => state.nodeParams[id] ?? ({} as Record<string, unknown>),
	);
	const setParam = useGraphStore((state) => state.setParam);

	const value = (params.capability as string) ?? "movement";

	const paramControls = (
		<div className="py-1">
			<div className="px-2 pb-1">
				<span className="block text-[10px] text-gray-400 mb-1">Capability</span>
				<Selector
					value={value}
					onChange={(newValue) => setParam(id, "capability", newValue)}
					align="start"
					placeholder="Select capability"
					options={CAPABILITY_OPTIONS}
				/>
			</div>
		</div>
	);

	return <BaseNode {...props} data={{ ...data, paramControls }} />;
}
