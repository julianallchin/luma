import type { NodeProps } from "reactflow";
import { useGraphStore } from "@/features/patterns/stores/use-graph-store";
import {
	Select,
	SelectContent,
	SelectItem,
	SelectTrigger,
	SelectValue,
} from "@/shared/components/ui/select";
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
			<div className="px-3 pb-1">
				<label
					htmlFor={`cap-${id}`}
					className="block text-[10px] text-gray-400 mb-1"
				>
					Capability
				</label>
				<Select
					value={value}
					onValueChange={(newValue) => setParam(id, "capability", newValue)}
				>
					<SelectTrigger id={`cap-${id}`} className="h-7 text-xs w-full">
						<SelectValue placeholder="Select capability" />
					</SelectTrigger>
					<SelectContent>
						{CAPABILITY_OPTIONS.map((option) => (
							<SelectItem
								key={option.value}
								value={option.value}
								className="text-xs"
							>
								{option.label}
							</SelectItem>
						))}
					</SelectContent>
				</Select>
			</div>
		</div>
	);

	return <BaseNode {...props} data={{ ...data, paramControls }} />;
}
