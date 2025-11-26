import type { NodeProps } from "reactflow";

import { usePatternAnnotationContext } from "@/features/patterns/contexts/pattern-annotation-context";
import { BaseNode, formatTime } from "./base-node";
import type { BaseNodeData } from "./types";

export function AudioInputNode(props: NodeProps<BaseNodeData>) {
	const { instances, selectedId } = usePatternAnnotationContext();
	const activeInstance =
		instances.find((inst) => inst.id === selectedId) ?? null;

	const trackName =
		activeInstance?.track.title ??
		activeInstance?.track.filePath ??
		"Select an annotation";
	const timeLabel =
		activeInstance != null
			? `${formatTime(activeInstance.startTime)} – ${formatTime(activeInstance.endTime)}`
			: null;

	const body = (
		<div className="text-[11px] space-y-1.5 max-w-48 px-2 pb-2">
			<div className="">
				<div className="text-[11px] font-medium text-foreground truncate">
					{trackName}
				</div>
				<div className="text-[10px] text-muted-foreground">
					{timeLabel ?? "Pick an instance from the left pane"}
				</div>
			</div>
		</div>
	);

	return <BaseNode {...props} data={{ ...props.data, body }} />;
}
