import { useEffect } from "react";
import { useAppViewStore } from "@/features/app/stores/use-app-view-store";
import { useStagePieceStore } from "../stores/use-stage-piece-store";
import { StageHierarchy } from "./stage-hierarchy";
import { StagePalette } from "./stage-palette";

/**
 * Left-side panel for the Universe Designer: palette of placeable stage
 * meshes on top, scene hierarchy of placed pieces below.
 */
export function StageBuilderPanel() {
	const venueId = useAppViewStore((s) => s.currentVenue?.id ?? null);
	const initialize = useStagePieceStore((s) => s.initialize);

	useEffect(() => {
		if (venueId) initialize(venueId);
	}, [venueId, initialize]);

	return (
		<div className="flex flex-col h-full min-h-0">
			<div className="flex-1 min-h-0 border-b border-trim">
				<StagePalette />
			</div>
			<div className="flex-1 min-h-0">
				<StageHierarchy />
			</div>
		</div>
	);
}
