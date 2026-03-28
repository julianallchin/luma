import { GitFork, Globe, Pencil, Search, Trash2 } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useLocation, useNavigate } from "react-router-dom";
import type { PatternSummary, SearchPatternRow } from "@/bindings/schema";
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
import { cn } from "@/shared/lib/utils";
import { useTrackEditorStore } from "../stores/use-track-editor-store";
import { fetchPreviewFrames, PreviewCanvas } from "./pattern-preview";

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

function getPatternColor(patternId: string): string {
	let hash = 0;
	for (let i = 0; i < patternId.length; i++) {
		hash = (hash * 31 + patternId.charCodeAt(i)) | 0;
	}
	return patternColors[Math.abs(hash) % patternColors.length];
}

const FILTER_TABS: { id: PatternFilter; label: string }[] = [
	{ id: "verified", label: "Verified" },
	{ id: "mine", label: "Mine" },
	{ id: "all", label: "All" },
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
	const searchResults = usePatternsStore((s) => s.searchResults);
	const searchLoading = usePatternsStore((s) => s.searchLoading);
	const searchQuery = usePatternsStore((s) => s.searchQuery);
	const searchRemote = usePatternsStore((s) => s.searchRemote);
	const setSearchQuery = usePatternsStore((s) => s.setSearchQuery);

	// Shared preview state — one Canvas reused across all hover previews
	const previewDataRef = useRef<{
		frames: import("@/bindings/universe").UniverseState[];
		durationSec: number;
	} | null>(null);
	const [_previewReady, setPreviewReady] = useState(false);
	const [previewAnchor, setPreviewAnchor] = useState<{
		patternId: string;
		description: string | null;
		rect: DOMRect;
	} | null>(null);

	const filteredPatterns = patterns.filter((p) => {
		if (filter === "mine") return p.uid === currentUserId;
		if (filter === "verified") return p.isVerified;
		return false; // "all" tab uses searchResults
	});

	const groupedPatterns = useMemo(() => {
		const groups: { category: string | null; patterns: PatternSummary[] }[] =
			[];
		const categoryMap = new Map<string | null, PatternSummary[]>();

		for (const p of filteredPatterns) {
			const key = p.categoryName ?? null;
			const group = categoryMap.get(key);
			if (group) {
				group.push(p);
			} else {
				categoryMap.set(key, [p]);
			}
		}

		// Named categories first (sorted), then uncategorized
		const sorted = [...categoryMap.entries()].sort(([a], [b]) => {
			if (a === null) return 1;
			if (b === null) return -1;
			return a.localeCompare(b);
		});

		for (const [category, pats] of sorted) {
			pats.sort((a, b) => a.name.localeCompare(b.name));
			groups.push({ category, patterns: pats });
		}

		return groups;
	}, [filteredPatterns]);

	const preloadRef = useRef<{ cancel: () => void } | null>(null);
	const closeTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

	const preloadPattern = useCallback(
		(patternId: string) => {
			// Cancel pending close — we're moving to a new item
			if (closeTimerRef.current) {
				clearTimeout(closeTimerRef.current);
				closeTimerRef.current = null;
			}

			const trackId = useTrackEditorStore.getState().trackId;
			const venueId = useTrackEditorStore.getState().venueId;
			const beatGrid = useTrackEditorStore.getState().beatGrid;
			const playheadPosition = useTrackEditorStore.getState().playheadPosition;
			if (!trackId || !venueId) return;

			// Cancel any in-flight preload
			preloadRef.current?.cancel();
			previewDataRef.current = null;
			setPreviewReady(false);

			const { promise, cancel } = fetchPreviewFrames(
				patternId,
				trackId,
				venueId,
				beatGrid,
				playheadPosition,
			);
			preloadRef.current = { cancel };

			promise.then((data) => {
				if (data) {
					previewDataRef.current = data;
					setPreviewReady(true);
				}
			});
		},
		[previewDataRef],
	);

	const openPreviewFor = useCallback(
		(patternId: string, description: string | null, el: HTMLElement) => {
			// Cancel pending close — we're opening on a new item
			if (closeTimerRef.current) {
				clearTimeout(closeTimerRef.current);
				closeTimerRef.current = null;
			}
			setPreviewAnchor({
				patternId,
				description,
				rect: el.getBoundingClientRect(),
			});
		},
		[],
	);

	const closePreviewPopover = useCallback(() => {
		// Delay close so moving between rows keeps the popover open
		closeTimerRef.current = setTimeout(() => {
			closeTimerRef.current = null;
			preloadRef.current?.cancel();
			preloadRef.current = null;
			setPreviewAnchor(null);
			previewDataRef.current = null;
			setPreviewReady(false);
		}, 150);
	}, [previewDataRef]);

	// Debounced remote search for the "all" tab
	const searchTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
	const handleSearchInput = useCallback(
		(value: string) => {
			setSearchQuery(value);
			if (searchTimerRef.current) clearTimeout(searchTimerRef.current);
			searchTimerRef.current = setTimeout(() => {
				searchRemote(value);
			}, 300);
		},
		[searchRemote, setSearchQuery],
	);

	// Trigger initial search when switching to "all" tab
	useEffect(() => {
		if (filter === "all" && searchResults.length === 0 && !searchLoading) {
			searchRemote(searchQuery);
		}
	}, [filter]);

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

			{/* Search input for "all" tab */}
			{filter === "all" && (
				<div className="px-2 pt-2">
					<div className="relative">
						<Search className="absolute left-2 top-1/2 -translate-y-1/2 w-3.5 h-3.5 text-muted-foreground" />
						<input
							type="text"
							value={searchQuery}
							onChange={(e) => handleSearchInput(e.target.value)}
							placeholder="Search patterns..."
							className="w-full pl-7 pr-2 py-1.5 text-xs bg-background border border-border/60 rounded-sm focus:outline-none focus:ring-1 focus:ring-ring"
						/>
					</div>
				</div>
			)}

			<div className="flex-1 overflow-y-auto">
				{filter === "all" ? (
					<AllTabContent
						results={searchResults}
						loading={searchLoading}
						backLabel={backLabel}
					/>
				) : filteredPatterns.length === 0 ? (
					<div className="p-4 text-xs text-muted-foreground text-center">
						<div className="opacity-50 mb-2">No patterns</div>
						{filter === "verified" && (
							<div className="text-[10px]">
								Verified patterns will appear here
							</div>
						)}
						{filter === "mine" && (
							<div className="text-[10px]">Create patterns in the Library</div>
						)}
					</div>
				) : (
					groupedPatterns.map(({ category, patterns: groupPatterns }) => (
						<div key={category ?? "__uncategorized"}>
							<div className="px-3 pt-1.5 pb-0.5">
								<span className="text-[10px] uppercase tracking-wide text-muted-foreground/70 font-medium">
									{category ?? "Uncategorized"}
								</span>
							</div>
							{groupPatterns.map((pattern) => (
								<PatternItem
									key={pattern.id}
									pattern={pattern}
									color={getPatternColor(pattern.id)}
									backLabel={backLabel}
									isOwner={pattern.uid === currentUserId}
									onDragStart={(origin) =>
										setDraggingPatternId(pattern.id, origin)
									}
									onDragEnd={() => {}}
									onPreviewPreload={preloadPattern}
									onPreviewOpen={openPreviewFor}
									onPreviewClose={closePreviewPopover}
									isPreviewVisible={previewAnchor !== null}
								/>
							))}
						</div>
					))
				)}
			</div>

			{/* Always-mounted preview popover — portaled to body, shown/hidden via style */}
			{createPortal(
				// biome-ignore lint/a11y/noStaticElementInteractions: hover continuation
				<div
					className="fixed z-50 w-72 rounded-md border bg-popover shadow-md overflow-hidden"
					style={
						previewAnchor
							? {
									top: previewAnchor.rect.top,
									left: previewAnchor.rect.right + 4,
									visibility: "visible" as const,
									pointerEvents: "auto" as const,
								}
							: {
									top: -9999,
									left: -9999,
									visibility: "hidden" as const,
									pointerEvents: "none" as const,
								}
					}
					onMouseEnter={() => {}}
					onMouseLeave={closePreviewPopover}
				>
					<div className="w-full h-40 relative">
						<PreviewCanvas previewDataRef={previewDataRef} />
					</div>
					{previewAnchor?.description && (
						<p className="text-muted-foreground text-xs px-3 py-2">
							{previewAnchor.description}
						</p>
					)}
				</div>,
				document.body,
			)}
		</div>
	);
}

// -- "All" tab: remote search results --

function AllTabContent({
	results,
	loading,
	backLabel,
}: {
	results: SearchPatternRow[];
	loading: boolean;
	backLabel: string;
}) {
	const navigate = useNavigate();
	const location = useLocation();
	const forkPattern = usePatternsStore((s) => s.forkPattern);

	if (loading) {
		return (
			<div className="p-4 text-xs text-muted-foreground text-center">
				Searching...
			</div>
		);
	}

	if (results.length === 0) {
		return (
			<div className="p-4 text-xs text-muted-foreground text-center">
				<div className="opacity-50 mb-2">No results</div>
				<div className="text-[10px]">
					Search for patterns used by other users
				</div>
			</div>
		);
	}

	return (
		<>
			{results.map((pat) => (
				<div
					key={pat.id}
					className="group w-full flex items-center gap-2 px-3 py-2 hover:bg-muted/50 transition-colors duration-150 hover:duration-0 select-none"
				>
					<div className="relative w-3 h-3 flex-shrink-0">
						<div
							className="w-3 h-3 rounded-sm"
							style={{ backgroundColor: getPatternColor(pat.id) }}
						/>
					</div>
					<div className="flex-1 min-w-0 text-left">
						<div className="text-xs font-medium truncate text-foreground/90">
							{pat.name}
						</div>
						{pat.authorName && (
							<div className="text-[10px] text-muted-foreground truncate">
								by {pat.authorName}
							</div>
						)}
					</div>
					<button
						type="button"
						onClick={async () => {
							try {
								const forked = await forkPattern(pat.id);
								navigate(`/pattern/${forked.id}`, {
									state: {
										name: forked.name,
										from: `${location.pathname}${location.search}`,
										backLabel,
									},
								});
							} catch (err) {
								console.error("Failed to fork pattern", err);
							}
						}}
						className="opacity-0 group-hover:opacity-70 text-muted-foreground hover:text-foreground transition-colors p-1 rounded hover:bg-muted"
						aria-label={`Fork ${pat.name}`}
					>
						<GitFork className="w-3.5 h-3.5" />
					</button>
				</div>
			))}
		</>
	);
}

// -- Pattern item (for Verified/Mine tabs) --

type PatternItemProps = {
	pattern: PatternSummary;
	color: string;
	backLabel: string;
	isOwner: boolean;
	onDragStart: (origin: { x: number; y: number }) => void;
	onDragEnd: () => void;
	onPreviewPreload: (patternId: string) => void;
	onPreviewOpen: (
		patternId: string,
		description: string | null,
		el: HTMLElement,
	) => void;
	onPreviewClose: () => void;
	isPreviewVisible: boolean;
};

function PatternItem({
	pattern,
	color,
	backLabel,
	isOwner,
	onDragStart,
	onPreviewPreload,
	onPreviewOpen,
	onPreviewClose,
	isPreviewVisible,
}: PatternItemProps) {
	const navigate = useNavigate();
	const location = useLocation();
	const filter = usePatternsStore((s) => s.filter);
	const forkPattern = usePatternsStore((s) => s.forkPattern);
	const deletePattern = usePatternsStore((s) => s.deletePattern);
	const loadPatterns = useTrackEditorStore((s) => s.loadPatterns);

	const hoverTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
	const rowRef = useRef<HTMLDivElement>(null);

	const openPreview = useCallback(() => {
		// Start fetching immediately
		onPreviewPreload(pattern.id);

		if (isPreviewVisible) {
			// Popover already open — swap instantly
			if (rowRef.current) {
				onPreviewOpen(pattern.id, pattern.description, rowRef.current);
			}
		} else {
			// First hover — show after delay
			hoverTimerRef.current = setTimeout(() => {
				if (rowRef.current) {
					onPreviewOpen(pattern.id, pattern.description, rowRef.current);
				}
			}, 300);
		}
	}, [
		pattern.id,
		pattern.description,
		onPreviewPreload,
		onPreviewOpen,
		isPreviewVisible,
	]);

	const closePreview = useCallback(() => {
		if (hoverTimerRef.current) {
			clearTimeout(hoverTimerRef.current);
			hoverTimerRef.current = null;
		}
		onPreviewClose();
	}, [onPreviewClose]);

	const handleMouseDown = (e: React.MouseEvent) => {
		if (e.button !== 0) return; // Only left click
		closePreview();
		onDragStart({ x: e.clientX, y: e.clientY });
	};

	const navigateToPattern = (id: string, name: string) => {
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
				{/* biome-ignore lint/a11y/noStaticElementInteractions: hover preview trigger */}
				<div
					ref={rowRef}
					className="relative"
					onMouseEnter={openPreview}
					onMouseLeave={closePreview}
				>
					{/* biome-ignore lint/a11y/useSemanticElements: drag handle needs div for mousedown */}
					<div
						role="button"
						tabIndex={0}
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
							{isOwner && pattern.isVerified && (
								<Globe className="absolute -top-1 -right-1 w-2 h-2 text-primary" />
							)}
						</div>

						{/* Pattern name + author */}
						<div className="flex-1 min-w-0 text-left">
							<div className="text-xs font-medium truncate text-foreground/90">
								{pattern.name}
							</div>
							{isOwner
								? filter === "verified" && (
										<div className="text-[10px] text-muted-foreground truncate">
											by you
										</div>
									)
								: pattern.authorName && (
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
					</div>
				</div>
			</ContextMenuTrigger>
			<ContextMenuContent>
				{isOwner && (
					<ContextMenuItem
						onClick={() => navigateToPattern(pattern.id, pattern.name)}
					>
						<Pencil className="w-3.5 h-3.5" />
						Edit
					</ContextMenuItem>
				)}
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
				<ContextMenuSeparator />
				<ContextMenuItem variant="destructive" onClick={handleDelete}>
					<Trash2 className="w-3.5 h-3.5" />
					Delete
				</ContextMenuItem>
			</ContextMenuContent>
		</ContextMenu>
	);
}
