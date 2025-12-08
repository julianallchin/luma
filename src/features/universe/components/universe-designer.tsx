import { useEffect } from "react";
import { useFixtureStore } from "../stores/use-fixture-store";
import { AssignmentMatrix } from "./assignment-matrix";
import { DmxChannelPane } from "./dmx-channel-pane";
import { PatchSchedule } from "./patch-schedule";
import { SimulationPane } from "./simulation-pane";
import { SourcePane } from "./source-pane";

export function UniverseDesigner() {
	const initialize = useFixtureStore((state) => state.initialize);

	useEffect(() => {
		initialize();
	}, [initialize]);

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

				{/* Right Sidebar: DMX Overrides + Patch Schedule */}
				<div className="w-80 border-l border-border flex flex-col h-full">
					<DmxChannelPane />
					<PatchSchedule className="flex-1 h-1/2 border-l-0" />
				</div>
			</div>
		</div>
	);
}
