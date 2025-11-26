import type { NodeProps } from "reactflow";

import { usePatternAnnotationContext } from "@/features/patterns/contexts/pattern-annotation-context";
import { BaseNode } from "./base-node";
import type { BaseNodeData } from "./types";

export function BeatClockNode(props: NodeProps<BaseNodeData>) {
	const { instances, selectedId, loading } = usePatternAnnotationContext();
	const activeInstance = instances.find((inst) => inst.id === selectedId) ?? null;
	const beatGrid = activeInstance?.beatGrid;

	const barsLabel =
		beatGrid != null
			? `${beatGrid.beatsPerBar} beats / bar, offset ${beatGrid.downbeatOffset.toFixed(2)}s`
			: "No beat grid available";
	const bpmLabel =
		beatGrid != null ? `${Math.round(beatGrid.bpm * 100) / 100} BPM` : "--";

	const body = (
		<div className="px-2 pb-2 text-[11px] space-y-1.5">
			<div className="flex items-center justify-between">
				<span className="uppercase tracking-wide text-muted-foreground text-[10px]">
					Beat Clock
				</span>
				<span className="text-[10px] text-muted-foreground">
					{loading ? "Loading context..." : "Annotation-provided"}
				</span>
			</div>
			<div className="rounded bg-card border border-border px-2 py-2 space-y-1">
				<div className="text-[11px] font-medium text-foreground">
					{bpmLabel}
				</div>
				<div className="text-[10px] text-muted-foreground">{barsLabel}</div>
				{!beatGrid && (
					<p className="text-[10px] text-primary">
						Add beats to the track annotation to drive tempo-aware nodes.
					</p>
				)}
			</div>
		</div>
	);

	return <BaseNode {...props} data={{ ...props.data, body }} />;
}
