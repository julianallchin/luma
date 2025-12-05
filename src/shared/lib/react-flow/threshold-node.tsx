import type { NodeProps } from "reactflow";
import { useGraphStore } from "@/features/patterns/stores/use-graph-store";
import { Label } from "@/shared/components/ui/label";
import { Slider } from "@/shared/components/ui/slider";
import { BaseNode } from "./base-node";
import type { BaseNodeData } from "./types";

export function ThresholdNode(props: NodeProps<BaseNodeData>) {
	const { data, id } = props;
	const params = useGraphStore(
		(state) => state.nodeParams[id] ?? ({} as Record<string, unknown>),
	);
	const setParam = useGraphStore((state) => state.setParam);

	const threshold =
		(params.threshold as number) ??
		(typeof data.params.threshold === "number"
			? (data.params.threshold as number)
			: 0.5);

	const handleChange = (next: number) => {
		const clamped = Math.min(1, Math.max(0, next));
		setParam(id, "threshold", clamped);
	};

	const paramControls = (
		<div className="p-2 space-y-2">
			<div className="flex items-center justify-between">
				<Label
					htmlFor={`${id}-threshold`}
					className="text-[10px] text-muted-foreground"
				>
					Threshold
				</Label>
				<span className="text-[10px] font-mono text-muted-foreground">
					{threshold.toFixed(2)}
				</span>
			</div>

			<div
				className="nodrag"
				onPointerDown={(e) => {
					// Keep slider interaction from dragging the node
					e.stopPropagation();
				}}
			>
				<Slider
					id={`${id}-threshold`}
					min={0}
					max={1}
					step={0.01}
					value={threshold}
					onChange={(e) => handleChange(Number(e.target.value))}
				/>
			</div>

			<div className="flex justify-between text-[9px] text-muted-foreground">
				<span>0</span>
				<span>1</span>
			</div>
		</div>
	);

	return <BaseNode {...props} data={{ ...data, paramControls }} />;
}
