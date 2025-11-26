import type { NodeProps } from "reactflow";

import { usePatternAnnotationContext } from "@/features/patterns/contexts/pattern-annotation-context";
import { BaseNode } from "./base-node";
import type { BeatClockNodeData } from "./types";

export function BeatClockNode(props: NodeProps<BeatClockNodeData>) {
	const { instances, selectedId } = usePatternAnnotationContext();
	const activeInstance =
		instances.find((inst) => inst.id === selectedId) ?? null;
	const beatGrid = activeInstance?.beatGrid;
	const bpmLabel =
		props.data.bpmLabel ??
		(beatGrid != null ? `${Math.round(beatGrid.bpm * 100) / 100} BPM` : "--");

	const body = (
		<div className="text-[11px] space-y-1.5 p-2 pt-0">
			<div className="text-[11px] font-medium text-foreground">{bpmLabel}</div>
		</div>
	);

	return <BaseNode {...props} data={{ ...props.data, body }} />;
}
