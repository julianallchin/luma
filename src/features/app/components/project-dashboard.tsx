import { useNavigate, useSearchParams } from "react-router-dom";
import { cn } from "@/shared/lib/utils";
import { PatternList } from "../../patterns/components/pattern-list";
import { TrackList } from "../../tracks/components/track-list";

type ViewMode = "patterns" | "tracks";

export function ProjectDashboard() {
	const [searchParams, setSearchParams] = useSearchParams();
	const tabParam = searchParams.get("tab");
	const activeView: ViewMode = tabParam === "tracks" ? "tracks" : "patterns";
	const navigate = useNavigate();

	const setActiveView = (view: ViewMode) => {
		const next = new URLSearchParams(searchParams);
		next.set("tab", view);
		if (view !== "patterns") {
			next.delete("category");
		}
		setSearchParams(next, { replace: true });
	};

	return (
		<div className="flex h-full w-full bg-card text-foreground">
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
						<SidebarItem active={false} onClick={() => navigate("/universe")}>
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
					? "bg-muted text-foreground font-medium"
					: "text-muted-foreground hover:bg-input",
			)}
		>
			{children}
		</button>
	);
}
