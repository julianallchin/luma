import type { NodeProps } from "reactflow";
import { useGraphStore } from "@/features/patterns/stores/use-graph-store";
import { Checkbox } from "@/shared/components/ui/checkbox";
import { Label } from "@/shared/components/ui/label";
import { Slider } from "@/shared/components/ui/slider";
import { cn } from "@/shared/lib/utils";
import { BaseNode } from "./base-node";
import type { BaseNodeData } from "./types";

const SUBDIVISIONS = [0.25, 0.5, 1, 2, 4];
const SUBDIVISION_LABELS: Record<number, string> = {
	0.25: "1/4",
	0.5: "1/2",
	1: "1",
	2: "2",
	4: "4",
};

export function BeatEnvelopeNode(props: NodeProps<BaseNodeData>) {
	const { data, id } = props;
	const params = useGraphStore(
		(state) => state.nodeParams[id] ?? ({} as Record<string, unknown>),
	);
	const setParam = useGraphStore((state) => state.setParam);

	const getNum = (key: string, def: number) => (params[key] as number) ?? def;
	const getBool = (key: string, def: boolean) =>
		(params[key] as number) === 1.0 || ((params[key] as boolean) ?? def);

	const updateNum = (key: string, val: number) => setParam(id, key, val);
	const updateBool = (key: string, val: boolean) =>
		setParam(id, key, val ? 1.0 : 0.0);

	// Parameter Controls
	const renderSlider = (
		key: string,
		label: string,
		min: number,
		max: number,
		step: number,
		def: number,
	) => {
		const val = getNum(key, def);
		return (
			<div className="space-y-1">
				<div className="flex items-center justify-between">
					<Label
						htmlFor={`${id}-${key}`}
						className="text-[10px] text-muted-foreground"
					>
						{label}
					</Label>
					<span className="text-[10px] font-mono text-muted-foreground">
						{val.toFixed(2)}
					</span>
				</div>
				<div
					className="flex items-center gap-2 nodrag"
					onPointerDown={(e) => e.stopPropagation()}
				>
					<Slider
						id={`${id}-${key}`}
						min={min}
						max={max}
						step={step}
						value={val}
						onChange={(e) => updateNum(key, Number(e.target.value))}
						className="flex-1"
					/>
				</div>
			</div>
		);
	};

	const paramControls = (
		<div className="flex flex-col gap-1 p-1">
			{/* Subdivision Segmented Control */}
			<div className="space-y-1">
				<Label className="text-[10px] text-muted-foreground">Subdivision</Label>
				<div className="flex rounded-md bg-input p-0.5">
					{SUBDIVISIONS.map((sub) => {
						const current = getNum("subdivision", 1.0);
						const isActive = Math.abs(current - sub) < 0.01;
						return (
							<button
								key={sub}
								type="button"
								onClick={() => updateNum("subdivision", sub)}
								className={cn(
									"flex-1 rounded-sm px-1 py-0.5 text-[10px] font-medium transition-all",
									isActive
										? "bg-background text-foreground shadow-sm"
										: "text-muted-foreground hover:text-foreground",
								)}
							>
								{SUBDIVISION_LABELS[sub]}
							</button>
						);
					})}
				</div>
			</div>

			{/* Downbeats Checkbox */}
			<div className="flex items-center gap-2">
				<Checkbox
					id={`${id}-only_downbeats`}
					checked={getBool("only_downbeats", false)}
					onCheckedChange={(c) => updateBool("only_downbeats", c === true)}
				/>
				<Label
					htmlFor={`${id}-only_downbeats`}
					className="text-xs cursor-pointer select-none"
				>
					Only Downbeats
				</Label>
			</div>

			<div className="h-px bg-border/50 my-1" />

			{/* Envelope Sliders */}
			<div className="grid gap-2">
				{renderSlider("amplitude", "Amplitude", 0, 2, 0.01, 1.0)}
				{renderSlider("offset", "Offset (Beats)", -1, 1, 0.01, 0.0)}
			</div>

			<div className="h-px bg-border/50 my-1" />

			{/* ADSR */}
			<div className="grid gap-2">
				{renderSlider("attack", "Attack", 0, 1, 0.01, 0.3)}
				{renderSlider("decay", "Decay", 0, 1, 0.01, 0.2)}
				{renderSlider("sustain", "Sustain Hold", 0, 1, 0.01, 0.3)}
				{renderSlider("release", "Release", 0, 1, 0.01, 0.2)}
				{renderSlider("sustain_level", "Sustain Level", 0, 1, 0.01, 0.7)}
			</div>

			<div className="h-px bg-border/50 my-1" />

			{/* Curves */}
			<div className="grid gap-2">
				{renderSlider("attack_curve", "Attack Curve", -1, 1, 0.01, 0.0)}
				{renderSlider("decay_curve", "Decay Curve", -1, 1, 0.01, 0.0)}
			</div>
		</div>
	);

	return <BaseNode {...props} data={{ ...data, paramControls }} />;
}
