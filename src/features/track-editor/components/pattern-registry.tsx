import { GitFork, Globe, Pencil, Trash2 } from "lucide-react";
import { useLocation, useNavigate } from "react-router-dom";
import type { PatternSummary } from "@/bindings/schema";
import { useAuthStore } from "@/features/auth/stores/use-auth-store";
import {
	type PatternFilter,
	usePatternsStore,
} from "@/features/patterns/stores/use-patterns-store";
import {
	ContextMenu,
	ContextMenuContent,
	ContextMenuItem,
	ContextMenuSeparator,
	ContextMenuTrigger,
} from "@/shared/components/ui/context-menu";
import {
	HoverCard,
	HoverCardContent,
	HoverCardTrigger,
} from "@/shared/components/ui/hover-card";
import { cn } from "@/shared/lib/utils";
import { useTrackEditorStore } from "../stores/use-track-editor-store";

const patternColors = [
	"#8b5cf6",
	"#ec4899",
	"#f59e0b",
	"#10b981",
	"#3b82f6",
	"#ef4444",
	"#06b6d4",
	"#f97316",
];

function getPatternColor(patternId: number): string {
	return patternColors[patternId % patternColors.length];
}

const FILTER_TABS: { id: PatternFilter; label: string }[] = [
	{ id: "all", label: "All" },
	{ id: "mine", label: "Mine" },
	{ id: "community", label: "Community" },
];

export function PatternRegistry() {
	const patterns = useTrackEditorStore((s) => s.patterns);
	const patternsLoading = useTrackEditorStore((s) => s.patternsLoading);
	const setDraggingPatternId = useTrackEditorStore(
		(s) => s.setDraggingPatternId,
	);
	const trackName = useTrackEditorStore((s) => s.trackName);
	const backLabel = trackName || "Track";
	const currentUserId = useAuthStore((s) => s.user?.id ?? null);
	const filter = usePatternsStore((s) => s.filter);
	const setFilter = usePatternsStore((s) => s.setFilter);

	const filteredPatterns = patterns.filter((p) => {
		if (filter === "all") return true;
		if (filter === "mine") return p.uid === currentUserId;
		return p.uid !== currentUserId;
	});

	if (patternsLoading) {
		return (
			<div className="p-4 text-xs text-muted-foreground">
				Loading patterns...
			</div>
		);
	}

	return (
		<div className="flex flex-col h-full">
			{/* Filter tabs */}
			<div className="px-2 pt-2">
				<div
					className="flex items-center border border-border/60 bg-background/70 p-0.5 text-[11px] font-medium"
					role="tablist"
					aria-label="Pattern filter"
				>
					{FILTER_TABS.map((tab) => (
						<button
							key={tab.id}
							type="button"
							role="tab"
							aria-selected={filter === tab.id}
							onClick={() => setFilter(tab.id)}
							className={cn(
								"flex-1 px-2.5 py-1 transition-colors",
								filter === tab.id
									? "bg-foreground text-background"
									: "text-muted-foreground hover:text-foreground",
							)}
						>
							{tab.label}
						</button>
					))}
				</div>
			</div>

			<div className="flex-1 overflow-y-auto">
				{filteredPatterns.length === 0 ? (
					<div className="p-4 text-xs text-muted-foreground text-center">
						<div className="opacity-50 mb-2">No patterns</div>
						{filter === "community" && (
							<div className="text-[10px]">
								Published patterns from other users will appear here
							</div>
						)}
						{filter === "mine" && (
							<div className="text-[10px]">Create patterns in the Library</div>
						)}
					</div>
				) : (
					filteredPatterns.map((pattern) => (
						<PatternItem
							key={pattern.id}
							pattern={pattern}
							color={getPatternColor(pattern.id)}
							backLabel={backLabel}
							isOwner={pattern.uid === currentUserId}
							onDragStart={(origin) => setDraggingPatternId(pattern.id, origin)}
							onDragEnd={() => {}}
						/>
					))
				)}
			</div>
		</div>
	);
}

type PatternItemProps = {
	pattern: PatternSummary;
	color: string;
	backLabel: string;
	isOwner: boolean;
	onDragStart: (origin: { x: number; y: number }) => void;
	onDragEnd: () => void;
};

function PatternItem({
	pattern,
	color,
	backLabel,
	isOwner,
	onDragStart,
}: PatternItemProps) {
	const navigate = useNavigate();
	const location = useLocation();
	const forkPattern = usePatternsStore((s) => s.forkPattern);
	const deletePattern = usePatternsStore((s) => s.deletePattern);
	const loadPatterns = useTrackEditorStore((s) => s.loadPatterns);

	const handleMouseDown = (e: React.MouseEvent) => {
		if (e.button !== 0) return; // Only left click
		onDragStart({ x: e.clientX, y: e.clientY });
	};

	const navigateToPattern = (id: number, name: string) => {
		navigate(`/pattern/${id}`, {
			state: {
				name,
				from: `${location.pathname}${location.search}`,
				backLabel,
			},
		});
	};

	const handleEditClick = (e: React.MouseEvent) => {
		e.stopPropagation();
		navigateToPattern(pattern.id, pattern.name);
	};

	const handleForkClick = async (e: React.MouseEvent) => {
		e.stopPropagation();
		try {
			const forked = await forkPattern(pattern.id);
			navigateToPattern(forked.id, forked.name);
		} catch (err) {
			console.error("Failed to fork pattern", err);
		}
	};

	const handleDelete = async () => {
		try {
			await deletePattern(pattern.id);
			await loadPatterns();
		} catch (err) {
			console.error("Failed to delete pattern", err);
		}
	};

	return (
		<ContextMenu>
			<ContextMenuTrigger asChild>
				<div>
					<HoverCard openDelay={300} closeDelay={100}>
						<HoverCardTrigger asChild>
							<button
								type="button"
								aria-label="Drag to add pattern"
								onMouseDown={handleMouseDown}
								className="group w-full flex items-center gap-2 px-3 py-2 cursor-grab active:cursor-grabbing hover:bg-muted/50 transition-colors duration-150 hover:duration-0 select-none"
							>
								{/* Color indicator */}
								<div className="relative w-3 h-3 flex-shrink-0">
									<div
										className="w-3 h-3 rounded-sm"
										style={{ backgroundColor: color }}
									/>
									{isOwner && pattern.isPublished && (
										<Globe className="absolute -top-1 -right-1 w-2 h-2 text-primary" />
									)}
								</div>

								{/* Pattern name + author */}
								<div className="flex-1 min-w-0 text-left">
									<div className="text-xs font-medium truncate text-foreground/90">
										{pattern.name}
									</div>
									{!isOwner && pattern.authorName && (
										<div className="text-[10px] text-muted-foreground truncate">
											by {pattern.authorName}
										</div>
									)}
								</div>

								{/* Edit or Fork button */}
								{isOwner ? (
									<button
										type="button"
										onMouseDown={(e) => e.stopPropagation()}
										onClick={handleEditClick}
										className="opacity-0 group-hover:opacity-70 text-muted-foreground hover:text-foreground transition-colors p-1 rounded hover:bg-muted"
										aria-label={`Edit ${pattern.name}`}
									>
										<Pencil className="w-3.5 h-3.5" />
									</button>
								) : (
									<button
										type="button"
										onMouseDown={(e) => e.stopPropagation()}
										onClick={handleForkClick}
										className="opacity-0 group-hover:opacity-70 text-muted-foreground hover:text-foreground transition-colors p-1 rounded hover:bg-muted"
										aria-label={`Fork ${pattern.name}`}
									>
										<GitFork className="w-3.5 h-3.5" />
									</button>
								)}
							</button>
						</HoverCardTrigger>
						{pattern.description && (
							<HoverCardContent
								side="right"
								align="start"
								className="w-56 text-xs"
							>
								<p className="text-muted-foreground">{pattern.description}</p>
							</HoverCardContent>
						)}
					</HoverCard>
				</div>
			</ContextMenuTrigger>
			<ContextMenuContent>
				{isOwner ? (
					<ContextMenuItem
						onClick={() => navigateToPattern(pattern.id, pattern.name)}
					>
						<Pencil className="w-3.5 h-3.5" />
						Edit
					</ContextMenuItem>
				) : (
					<ContextMenuItem
						onClick={async () => {
							try {
								const forked = await forkPattern(pattern.id);
								navigateToPattern(forked.id, forked.name);
							} catch (err) {
								console.error("Failed to fork pattern", err);
							}
						}}
					>
						<GitFork className="w-3.5 h-3.5" />
						Fork
					</ContextMenuItem>
				)}
				<ContextMenuSeparator />
				<ContextMenuItem variant="destructive" onClick={handleDelete}>
					<Trash2 className="w-3.5 h-3.5" />
					Delete
				</ContextMenuItem>
			</ContextMenuContent>
		</ContextMenu>
	);
}
