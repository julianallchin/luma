import type { NodeProps } from "reactflow";
import { useGraphStore } from "@/features/patterns/stores/use-graph-store";
import { Label } from "@/shared/components/ui/label";
import { Slider } from "@/shared/components/ui/slider";
import { BaseNode } from "./base-node";
import type { BaseNodeData } from "./types";

export function FalloffNode(props: NodeProps<BaseNodeData>) {
	const { data, id } = props;
	const params = useGraphStore(
		(state) => state.nodeParams[id] ?? ({} as Record<string, unknown>),
	);
	const setParam = useGraphStore((state) => state.setParam);

	const getNum = (key: string, def: number) => {
		const fromState = params[key] as number | undefined;
		if (typeof fromState === "number") return fromState;
		const fromData = data.params[key];
		return typeof fromData === "number" ? (fromData as number) : def;
	};

	const width = getNum("width", 1);
	const curve = getNum("curve", 0);

	const updateNum = (key: string, val: number) => setParam(id, key, val);

	const paramControls = (
		<div className="p-2 space-y-2 text-[11px] max-w-48">
			<p className="text-muted-foreground">
				Softly tightens a 0..1 signal (or distance) into a pill-shaped falloff.
			</p>

			<div className="space-y-1">
				<div className="flex items-center justify-between">
					<Label
						htmlFor={`${id}-width`}
						className="text-[10px] text-muted-foreground"
					>
						Width
					</Label>
					<span className="text-[10px] font-mono text-muted-foreground">
						{width.toFixed(2)}
					</span>
				</div>
				<div
					className="nodrag"
					onPointerDown={(e) => {
						e.stopPropagation();
					}}
				>
					<Slider
						id={`${id}-width`}
						min={0}
						max={4}
						step={0.01}
						value={width}
						onChange={(e) => updateNum("width", Number(e.target.value))}
					/>
				</div>
				<p className="text-[9px] text-muted-foreground">
					Higher = tighter pill; lower = wider falloff.
				</p>
			</div>

			<div className="space-y-1">
				<div className="flex items-center justify-between">
					<Label
						htmlFor={`${id}-curve`}
						className="text-[10px] text-muted-foreground"
					>
						Curve
					</Label>
					<span className="text-[10px] font-mono text-muted-foreground">
						{curve.toFixed(2)}
					</span>
				</div>
				<div
					className="nodrag"
					onPointerDown={(e) => {
						e.stopPropagation();
					}}
				>
					<Slider
						id={`${id}-curve`}
						min={-1}
						max={1}
						step={0.01}
						value={curve}
						onChange={(e) => updateNum("curve", Number(e.target.value))}
					/>
				</div>
				<p className="text-[9px] text-muted-foreground">
					Negative = softer edges, positive = snappier edges.
				</p>
			</div>
		</div>
	);

	return <BaseNode {...props} data={{ ...data, paramControls }} />;
}
