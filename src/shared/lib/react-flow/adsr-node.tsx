import * as React from "react";
import type { NodeProps } from "reactflow";
import { useGraphStore } from "@/features/patterns/stores/use-graph-store";
import { Checkbox } from "@/shared/components/ui/checkbox";
import { Label } from "@/shared/components/ui/label";
import { Slider } from "@/shared/components/ui/slider";
import { BaseNode } from "./base-node";
import { EnvelopeCanvas, type ParamUpdates } from "./beat-envelope-node";
import type { BaseNodeData } from "./types";

export function AdsrNode(props: NodeProps<BaseNodeData>) {
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

	const handleEnvelopeChange = React.useCallback(
		(updates: ParamUpdates) => {
			useGraphStore.setState((state) => {
				const existing = state.nodeParams[id] ?? {};
				return {
					nodeParams: {
						...state.nodeParams,
						[id]: { ...existing, ...updates },
					},
					version: state.version + 1,
				};
			});
		},
		[id],
	);

	const fitToGap = getBool("fit_to_gap", false);
	const lengthBeats = getNum("length_beats", 0.5);

	const paramControls = (
		<div
			className="flex flex-col gap-1 p-1"
			onPointerDown={(e) => e.stopPropagation()}
		>
			<EnvelopeCanvas
				attack={getNum("attack", 0.1)}
				decay={getNum("decay", 0.4)}
				sustain={getNum("sustain", 0.0)}
				release={getNum("release", 0.5)}
				sustainLevel={getNum("sustain_level", 0.5)}
				attackCurve={getNum("attack_curve", 0.0)}
				decayCurve={getNum("decay_curve", 0.0)}
				onChange={handleEnvelopeChange}
			/>

			<div className="h-px bg-border -mx-2" />

			<div className="flex items-center gap-2">
				<Checkbox
					id={`${id}-fit_to_gap`}
					checked={fitToGap}
					onCheckedChange={(c) => updateBool("fit_to_gap", c === true)}
				/>
				<Label
					htmlFor={`${id}-fit_to_gap`}
					className="text-xs cursor-pointer select-none"
					title="When on, the envelope spans the gap between consecutive events (best for periodic pulses). When off, it spans `Length` beats (best for sparse drum hits)."
				>
					Fit To Gap
				</Label>
			</div>

			{!fitToGap && (
				<div className="space-y-0.5">
					<Label className="text-[10px] text-muted-foreground">
						Length: {lengthBeats.toFixed(2)} beats
					</Label>
					<div className="nodrag">
						<Slider
							id={`${id}-length_beats`}
							min={0.05}
							max={4}
							step={0.05}
							value={lengthBeats}
							onChange={(e) =>
								updateNum("length_beats", Number(e.target.value))
							}
							className="flex-1 h-4"
						/>
					</div>
				</div>
			)}

			<div className="space-y-0.5">
				<Label className="text-[10px] text-muted-foreground">Amplitude</Label>
				<div className="nodrag">
					<Slider
						id={`${id}-amplitude`}
						min={0}
						max={2}
						step={0.01}
						value={getNum("amplitude", 1.0)}
						onChange={(e) => updateNum("amplitude", Number(e.target.value))}
						className="flex-1 h-4"
					/>
				</div>
			</div>
		</div>
	);

	return <BaseNode {...props} data={{ ...data, paramControls }} />;
}
