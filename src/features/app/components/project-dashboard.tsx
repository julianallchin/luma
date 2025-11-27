import { useState } from "react";
import { cn } from "@/shared/lib/utils";
import { PatternList } from "../../patterns/components/pattern-list";
import { TrackList } from "../../tracks/components/track-list";
import { useAppViewStore } from "../stores/use-app-view-store";

type ViewMode = "patterns" | "tracks";

export function ProjectDashboard() {
	const [activeView, setActiveView] = useState<ViewMode>("patterns");
	const setView = useAppViewStore((state) => state.setView);

	return (
		<div className="flex h-full w-full bg-background text-foreground">
			{/* Sidebar */}
			<div className="w-64 border-r border-border flex flex-col">
				<div className="p-4">
					<h2 className="text-xs font-semibold text-muted-foreground mb-2 px-2">
						LIBRARY
					</h2>
					<div className="flex flex-col gap-1">
						<SidebarItem
							active={activeView === "patterns"}
							onClick={() => setActiveView("patterns")}
						>
							Patterns
						</SidebarItem>
						<SidebarItem
							active={activeView === "tracks"}
							onClick={() => setActiveView("tracks")}
						>
							Tracks
						</SidebarItem>
					</div>

					<h2 className="text-xs font-semibold text-muted-foreground mb-2 px-2 mt-4">
						SETUP
					</h2>
					<div className="flex flex-col gap-1">
						<SidebarItem
							active={false}
							onClick={() => setView({ type: "universe" })}
						>
							Universe Patch
						</SidebarItem>
					</div>
				</div>
			</div>

			{/* Main Content */}
			<div className="flex-1 min-w-0 bg-background/50">
				{activeView === "patterns" ? <PatternList /> : <TrackList />}
			</div>
		</div>
	);
}

function SidebarItem({
	children,
	active,
	onClick,
}: {
	children: React.ReactNode;
	active: boolean;
	onClick: () => void;
}) {
	return (
		<button
			type="button"
			onClick={onClick}
			className={cn(
				"text-sm text-left px-2 py-1.5 rounded-md",
				active
					? "bg-accent text-accent-foreground font-medium"
					: "text-muted-foreground hover:bg-muted hover:text-foreground",
			)}
		>
			{children}
		</button>
	);
}
