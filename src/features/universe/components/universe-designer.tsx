import { invoke } from "@tauri-apps/api/core";
import { useEffect, useRef } from "react";
import { dmxStore } from "@/features/visualizer/stores/dmx-store";
import { universeStore } from "@/features/visualizer/stores/universe-state-store";
import { useFixtureStore } from "../stores/use-fixture-store";
import { AssignmentMatrix } from "./assignment-matrix";
import { GroupedFixtureTree } from "./grouped-fixture-tree";
import { PatchSchedule } from "./patch-schedule";
import { SimulationPane } from "./simulation-pane";
import { SourcePane } from "./source-pane";

interface UniverseDesignerProps {
	venueId?: number;
}

export function UniverseDesigner({ venueId }: UniverseDesignerProps) {
	const initialize = useFixtureStore((state) => state.initialize);
	const lastSelectedPatchedId = useFixtureStore(
		(state) => state.lastSelectedPatchedId,
	);

	// Clear render engine + frontend caches so fixtures show as off
	useEffect(() => {
		invoke("render_clear_active_layer").catch(() => {});
		universeStore.clear();
		dmxStore.clear();
	}, []);

	// Blink-identify the fixture when selected
	const mountedRef = useRef(false);
	useEffect(() => {
		if (!mountedRef.current) {
			mountedRef.current = true;
			return;
		}
		if (lastSelectedPatchedId) {
			invoke("render_identify_fixture", {
				fixtureId: lastSelectedPatchedId,
			}).catch(() => {});
		}
	}, [lastSelectedPatchedId]);

	useEffect(() => {
		if (venueId !== undefined) {
			initialize(venueId);
		}
	}, [initialize, venueId]);

	return (
		<div className="flex h-full w-full bg-background text-foreground overflow-hidden">
			{/* Left Pane: Source (Search/List/Config) */}
			<div className="w-80 border-r border-border flex-shrink-0 flex flex-col">
				<SourcePane />
			</div>

			{/* Center + Right */}
			<div className="flex-1 flex h-full min-w-0">
				{/* Center Column */}
				<div className="flex-1 flex flex-col h-full min-w-0">
					{/* Top: Simulation */}
					<div className="h-1/2 border-b border-border relative">
						<SimulationPane />
					</div>

					{/* Bottom: Assignment Matrix */}
					<div className="h-1/2 relative">
						<AssignmentMatrix />
					</div>
				</div>

				{/* Right Sidebar: Patch Schedule → Groups → Tags */}
				<div className="w-80 border-l border-border flex flex-col h-full">
					{/* Fixtures list - draggable */}
					<PatchSchedule className="flex-1 min-h-0 border-l-0" />
					{/* Groups - drop targets + tags panel */}
					<div className="h-[45%] border-t border-border overflow-hidden">
						<GroupedFixtureTree />
					</div>
				</div>
			</div>
		</div>
	);
}
