import * as React from "react";
import type { NodeProps } from "reactflow";
import { useGraphStore } from "@/features/patterns/stores/use-graph-store";
import { useTracksStore } from "@/features/tracks/stores/use-tracks-store";
import {
	Select,
	SelectContent,
	SelectItem,
	SelectTrigger,
	SelectValue,
} from "@/shared/components/ui/select";
import { BaseNode } from "./base-node";
import type { BaseNodeData } from "./types";

export function AudioSourceNode(props: NodeProps<BaseNodeData>) {
	const { data } = props;
	const { tracks } = useTracksStore();
	const params = useGraphStore(
		(state) => state.nodeParams[props.id] ?? ({} as Record<string, unknown>),
	);
	const setParam = useGraphStore((state) => state.setParam);

	const rawTrackId = params.trackId;
	const selectedId =
		rawTrackId !== null && rawTrackId !== undefined ? Number(rawTrackId) : null;

	const validSelectedId =
		selectedId !== null &&
		!Number.isNaN(selectedId) &&
		tracks.some((t) => t.id === selectedId)
			? selectedId
			: null;

	React.useEffect(() => {
		if (validSelectedId === null && tracks.length > 0) {
			setParam(props.id, "trackId", tracks[0].id);
			data.onChange();
		}
	}, [tracks, validSelectedId, setParam, props.id, data]);

	const selectId = React.useId();

	// Ensure the selected value matches exactly with SelectItem values
	const selectValue =
		validSelectedId !== null && validSelectedId !== undefined
			? validSelectedId.toString()
			: "";

	const body = (
		<div className="px-2 pb-2">
			<label
				htmlFor={selectId}
				className="block text-[10px] text-gray-400 mb-1 uppercase tracking-wider"
			>
				Track
			</label>
			<Select
				value={selectValue}
				disabled={tracks.length === 0}
				onValueChange={(value) => {
					if (value === "") {
						setParam(props.id, "trackId", null);
						data.onChange();
						return;
					}
					const maybeId = parseInt(value, 10);
					setParam(props.id, "trackId", Number.isNaN(maybeId) ? null : maybeId);
					data.onChange();
				}}
			>
				<SelectTrigger id={selectId} className="w-full h-8 text-[11px]">
					<SelectValue placeholder="Select a track" />
				</SelectTrigger>
				<SelectContent>
					{tracks.map((track) => (
						<SelectItem key={track.id} value={track.id.toString()}>
							{track.title ?? `Track ${track.id}`}
						</SelectItem>
					))}
				</SelectContent>
			</Select>
		</div>
	);

	return <BaseNode {...props} data={{ ...data, body }} />;
}
