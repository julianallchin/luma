import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import {
	ChevronDown,
	Disc3,
	RefreshCw,
	RotateCcw,
	Search,
	Trash2,
	Upload,
} from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { toast } from "sonner";
import type { TrackBrowserRow, TrackSummary } from "@/bindings/schema";
import { useAppViewStore } from "@/features/app/stores/use-app-view-store";
import { useAuthStore } from "@/features/auth/stores/use-auth-store";
import { engineDjAdapter } from "@/features/dj-import/adapters/engine-dj";
import { rekordboxAdapter } from "@/features/dj-import/adapters/rekordbox";
import { DjImportBrowser } from "@/features/dj-import/components/dj-import-browser";
import { useDjImportStore } from "@/features/dj-import/stores/use-dj-import-store";
import type { TrackWaveform } from "@/features/track-editor/stores/use-track-editor-store";
import { useTrackEditorStore } from "@/features/track-editor/stores/use-track-editor-store";
import {
	AlertDialog,
	AlertDialogAction,
	AlertDialogCancel,
	AlertDialogContent,
	AlertDialogDescription,
	AlertDialogFooter,
	AlertDialogHeader,
	AlertDialogTitle,
} from "@/shared/components/ui/alert-dialog";
import { Button } from "@/shared/components/ui/button";
import {
	ContextMenu,
	ContextMenuContent,
	ContextMenuItem,
	ContextMenuTrigger,
} from "@/shared/components/ui/context-menu";
import {
	DropdownMenu,
	DropdownMenuContent,
	DropdownMenuItem,
	DropdownMenuTrigger,
} from "@/shared/components/ui/dropdown-menu";
import { cn } from "@/shared/lib/utils";
import { useTracksStore } from "../stores/use-tracks-store";
import { PreprocessingStatus } from "./preprocessing-status";
import { ScorePickerDialog } from "./score-picker-dialog";

const formatDuration = (seconds: number | null | undefined) => {
	if (seconds == null || Number.isNaN(seconds)) return "--:--";
	const total = Math.max(0, seconds);
	const minutes = Math.floor(total / 60);
	const secs = Math.floor(total % 60)
		.toString()
		.padStart(2, "0");
	return `${minutes}:${secs}`;
};

const getTrackName = (track: TrackBrowserRow) =>
	track.title || track.filePath.split("/").pop() || "Untitled";

export function TrackBrowser() {
	const browserTracks = useTracksStore((s) => s.browserTracks);
	const browserLoading = useTracksStore((s) => s.browserLoading);
	const searchQuery = useTracksStore((s) => s.searchQuery);
	const setSearchQuery = useTracksStore((s) => s.setSearchQuery);
	const refreshBrowser = useTracksStore((s) => s.refreshBrowser);
	const refresh = useTracksStore((s) => s.refresh);
	const activeTrackId = useTrackEditorStore((s) => s.trackId);
	const currentVenueId = useAppViewStore((s) => s.currentVenue?.id ?? null);
	const currentUserId = useAuthStore((s) => s.user?.id ?? null);

	const [importing, setImporting] = useState(false);
	const [scorePickerTrack, setScorePickerTrack] =
		useState<TrackBrowserRow | null>(null);
	const [djImportOpen, setDjImportOpen] = useState(false);
	const [deleteTrack, setDeleteTrack] = useState<TrackBrowserRow | null>(null);
	const openForSource = useDjImportStore((s) => s.openForSource);
	const [sourceFilter, setSourceFilter] = useState<"all" | "mine">("mine");
	const searchInputRef = useRef<HTMLInputElement>(null);
	const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);

	// Display name cache for other users' tracks
	const [displayNames, setDisplayNames] = useState<Record<string, string>>({});

	// Full load on mount
	useEffect(() => {
		refresh();
	}, [refresh]);

	// Reload browser tracks on mount and when venue changes
	// (ensures venueAnnotationCount is correct even if venue loads async after mount)
	useEffect(() => {
		refreshBrowser();
	}, [currentVenueId, refreshBrowser]);

	// Fetch display names for other users' tracks
	useEffect(() => {
		const otherUids = [
			...new Set(
				browserTracks
					.map((t) => t.uid)
					.filter((uid): uid is string => !!uid && uid !== currentUserId),
			),
		];
		// Only fetch uids we haven't resolved yet
		const missing = otherUids.filter((uid) => !(uid in displayNames));
		if (missing.length === 0) return;
		invoke<Record<string, string>>("get_display_names", { uids: missing })
			.then((names) => setDisplayNames((prev) => ({ ...prev, ...names })))
			.catch(() => {});
	}, [browserTracks, currentUserId]);

	// Refresh browser on track analysis completion or sync data changes (debounced)
	useEffect(() => {
		let timeout: ReturnType<typeof setTimeout> | null = null;
		const unsubs: (() => void)[] = [];
		let cancelled = false;

		const debouncedRefresh = () => {
			if (timeout) clearTimeout(timeout);
			timeout = setTimeout(() => {
				refreshBrowser();
			}, 500);
		};

		for (const event of ["track-status-changed", "library-changed"] as const) {
			listen(event, debouncedRefresh).then((unlisten) => {
				if (cancelled) unlisten();
				else unsubs.push(unlisten);
			});
		}

		return () => {
			cancelled = true;
			for (const unsub of unsubs) unsub();
			if (timeout) clearTimeout(timeout);
		};
	}, [refreshBrowser]);

	const handleSearchChange = (value: string) => {
		if (debounceRef.current) clearTimeout(debounceRef.current);
		debounceRef.current = setTimeout(() => {
			setSearchQuery(value);
		}, 300);
	};

	const filteredTracks = useMemo(() => {
		let result = browserTracks;
		if (sourceFilter === "mine") {
			result = result.filter((t) => !t.uid || t.uid === currentUserId);
		}
		if (searchQuery) {
			const q = searchQuery.toLowerCase();
			result = result.filter(
				(t) =>
					t.title?.toLowerCase().includes(q) ||
					t.artist?.toLowerCase().includes(q) ||
					t.album?.toLowerCase().includes(q),
			);
		}
		return result;
	}, [browserTracks, searchQuery, sourceFilter, currentUserId]);

	const handleImport = async () => {
		const selection = await open({
			multiple: true,
			directory: false,
			title: "Select tracks to import",
		});
		if (!selection) return;
		const files = Array.isArray(selection) ? selection : [selection];
		if (files.length === 0) return;

		setImporting(true);
		const toastId = "track-import";
		toast.loading(
			files.length === 1
				? `Importing ${files[0].split("/").pop()}…`
				: `Importing ${files.length} tracks…`,
			{ id: toastId },
		);

		let unlisten: UnlistenFn | null = null;
		try {
			unlisten = await listen<{
				done: number;
				total: number;
				currentTrack: string | null;
				phase: string;
				error: string | null;
			}>("file-import-progress", (event) => {
				const { done, total, currentTrack, error } = event.payload;
				if (error) {
					toast.error(error);
				} else if (currentTrack) {
					const prefix = total > 1 ? `[${done + 1}/${total}] ` : "";
					toast.loading(`${prefix}Importing ${currentTrack}…`, { id: toastId });
				}
			});

			const imported = await invoke<TrackSummary[]>("import_tracks", {
				filePaths: files,
			});
			await Promise.all([refresh(), refreshBrowser()]);

			if (imported.length === 0) {
				toast.error("Import failed", { id: toastId });
			} else {
				const failed = files.length - imported.length;
				const label =
					imported.length === 1 && files.length === 1
						? `Imported ${files[0].split("/").pop()}`
						: failed > 0
							? `Imported ${imported.length}/${files.length} tracks`
							: `Imported ${imported.length} tracks`;
				toast.success(label, { id: toastId });

				// Listen for background analysis progress (same as DJ import flow)
				const analysisToastId = "bg-analysis";
				toast.loading(
					`Analyzing ${imported.length} track${imported.length !== 1 ? "s" : ""}…`,
					{ id: analysisToastId },
				);
				const unlistenProgress = await listen<[string, string]>(
					"track-import-progress",
					(event) => {
						const [, step] = event.payload;
						toast.loading(step, { id: analysisToastId });
					},
				);
				const unlistenComplete = await listen<number>(
					"track-import-complete",
					() => {
						toast.success("Analysis complete", { id: analysisToastId });
						unlistenProgress();
						unlistenComplete();
						void refresh();
						void refreshBrowser();
					},
				);
			}
		} catch (err) {
			console.error("Failed to import tracks:", err);
			toast.error("Import failed", { id: toastId });
		} finally {
			unlisten?.();
			setImporting(false);
		}
	};

	const handleTrackSelect = (track: TrackBrowserRow) => {
		if (currentVenueId === null) return;
		setScorePickerTrack(track);
	};

	const handleDjImportClose = (open: boolean) => {
		setDjImportOpen(open);
		if (!open) {
			void refreshBrowser();
			void refresh();
		}
	};

	return (
		<div className="flex flex-col h-full bg-background">
			{/* Header */}
			<div className="flex items-center gap-3 px-4 py-3 border-b border-border/50">
				<div className="relative flex-1">
					<Search className="absolute left-2.5 top-1/2 -translate-y-1/2 h-3.5 w-3.5 text-muted-foreground" />
					<input
						ref={searchInputRef}
						type="text"
						placeholder="Search tracks..."
						defaultValue={searchQuery}
						onChange={(e) => handleSearchChange(e.target.value)}
						className="w-full h-8 pl-8 pr-3 text-xs bg-muted/50 border border-border/50 rounded-md focus:outline-none focus:ring-1 focus:ring-ring placeholder:text-muted-foreground/60"
					/>
				</div>
				<div className="flex items-center border border-border/60 bg-background/70 p-0.5 text-[10px] font-medium">
					{(
						[
							{ id: "mine", label: "Mine" },
							{ id: "all", label: "All" },
						] as const
					).map((opt) => (
						<button
							key={opt.id}
							type="button"
							onClick={() => setSourceFilter(opt.id)}
							className={cn(
								"px-2.5 py-1 transition-colors",
								sourceFilter === opt.id
									? "bg-foreground text-background"
									: "text-muted-foreground hover:text-foreground",
							)}
						>
							{opt.label}
						</button>
					))}
				</div>
				<DropdownMenu>
					<DropdownMenuTrigger asChild>
						<Button
							variant="outline"
							size="sm"
							className="h-8"
							disabled={importing}
						>
							Import
							<ChevronDown className="size-3 ml-1" />
						</Button>
					</DropdownMenuTrigger>
					<DropdownMenuContent align="end" className="w-56">
						<DropdownMenuItem onClick={handleImport}>
							<Upload className="size-4" />
							Upload Files
						</DropdownMenuItem>
						<DropdownMenuItem
							onClick={() => {
								openForSource(engineDjAdapter);
								setDjImportOpen(true);
							}}
						>
							<Disc3 className="size-4" />
							Import from Engine DJ
						</DropdownMenuItem>
						<DropdownMenuItem
							onClick={() => {
								openForSource(rekordboxAdapter);
								setDjImportOpen(true);
							}}
						>
							<Disc3 className="size-4" />
							Import from Rekordbox
						</DropdownMenuItem>
					</DropdownMenuContent>
				</DropdownMenu>
				<DjImportBrowser
					open={djImportOpen}
					onOpenChange={handleDjImportClose}
				/>
				{currentVenueId && (
					<ScorePickerDialog
						track={scorePickerTrack}
						venueId={currentVenueId}
						open={!!scorePickerTrack}
						onOpenChange={(open) => {
							if (!open) setScorePickerTrack(null);
						}}
					/>
				)}
			</div>

			{/* Column headers */}
			<div className="grid grid-cols-[40px_1fr_1fr_70px_60px_60px_70px] gap-2 px-4 py-2 text-[10px] font-medium text-muted-foreground uppercase select-none border-b border-border/30">
				<div />
				<div>Title</div>
				<div>Artist</div>
				<div className="text-right">BPM</div>
				<div className="text-right">Time</div>
				<div className="text-center">Status</div>
				<div className="text-right">Added By</div>
			</div>

			{/* Track rows */}
			<div className="flex-1 overflow-y-auto">
				{browserLoading && browserTracks.length === 0 ? (
					<div className="flex items-center justify-center h-32 text-xs text-muted-foreground">
						Loading tracks...
					</div>
				) : filteredTracks.length === 0 ? (
					<div className="flex flex-col items-center justify-center h-32 gap-2">
						<p className="text-xs text-muted-foreground">
							{searchQuery ? "No matching tracks" : "No tracks imported"}
						</p>
					</div>
				) : (
					filteredTracks.map((track) => {
						const isOwned = !track.uid || track.uid === currentUserId;
						const trackButton = (
							<button
								type="button"
								onClick={() => handleTrackSelect(track)}
								className={cn(
									"w-full grid grid-cols-[40px_1fr_1fr_70px_60px_60px_70px] gap-2 px-4 py-1.5 items-center text-left transition-colors duration-150 hover:duration-0",
									activeTrackId === track.id ? "bg-muted" : "hover:bg-muted",
								)}
							>
								{/* Album art */}
								<div className="relative h-8 w-8 overflow-hidden rounded bg-muted/50 flex-shrink-0">
									{track.albumArtData ? (
										<img
											src={track.albumArtData}
											alt=""
											className="h-full w-full object-cover"
										/>
									) : (
										<div className="w-full h-full flex items-center justify-center bg-muted text-[7px] text-muted-foreground uppercase tracking-tighter">
											No Art
										</div>
									)}
								</div>

								{/* Title */}
								<div className="text-xs font-medium text-foreground/90 truncate flex items-center gap-1.5">
									{track.venueAnnotationCount > 0 && (
										<span
											className="w-1.5 h-1.5 rounded-full bg-emerald-500 shrink-0"
											title={`${track.venueAnnotationCount} annotations for this venue`}
										/>
									)}
									{getTrackName(track)}
								</div>

								{/* Artist */}
								<div className="text-xs text-muted-foreground truncate">
									{track.artist || "Unknown artist"}
								</div>

								{/* BPM */}
								<div className="text-xs text-muted-foreground text-right font-mono">
									{track.bpm ? track.bpm.toFixed(1) : "--"}
								</div>

								{/* Duration */}
								<div className="text-xs text-muted-foreground text-right font-mono">
									{formatDuration(track.durationSeconds)}
								</div>

								{/* Preprocessing status */}
								<PreprocessingStatus track={track} />

								{/* Added by */}
								<div className="text-xs text-muted-foreground text-right">
									{track.uid && track.uid !== currentUserId
										? (displayNames[track.uid] ?? "shared")
										: "you"}
								</div>
							</button>
						);
						if (!isOwned) return <div key={track.id}>{trackButton}</div>;
						return (
							<ContextMenu key={track.id}>
								<ContextMenuTrigger asChild>{trackButton}</ContextMenuTrigger>
								<ContextMenuContent className="min-w-40">
									<ContextMenuItem
										onClick={() => {
											invoke("reprocess_track", {
												trackId: track.id,
											}).catch((err) =>
												console.error("Failed to reprocess track:", err),
											);
										}}
									>
										<RefreshCw className="size-4" />
										Reprocess
									</ContextMenuItem>
									<ContextMenuItem
										onClick={async () => {
											try {
												const waveform = await invoke<TrackWaveform>(
													"reprocess_waveform",
													{ trackId: track.id },
												);
												if (activeTrackId === track.id) {
													useTrackEditorStore.setState({
														waveform,
														durationSeconds: waveform.durationSeconds,
													});
												}
											} catch (err) {
												console.error("Failed to reprocess waveform:", err);
											}
										}}
									>
										<RotateCcw className="size-4" />
										Reprocess Waveform
									</ContextMenuItem>
									<ContextMenuItem
										variant="destructive"
										onClick={() => setDeleteTrack(track)}
									>
										<Trash2 className="size-4" />
										Delete
									</ContextMenuItem>
								</ContextMenuContent>
							</ContextMenu>
						);
					})
				)}
			</div>

			{/* Footer */}
			<div className="px-4 py-2 border-t border-border/30 text-[10px] text-muted-foreground">
				{filteredTracks.length} track{filteredTracks.length !== 1 ? "s" : ""}
			</div>

			<AlertDialog
				open={deleteTrack !== null}
				onOpenChange={(open) => {
					if (!open) setDeleteTrack(null);
				}}
			>
				<AlertDialogContent>
					<AlertDialogHeader>
						<AlertDialogTitle>Delete track</AlertDialogTitle>
						<AlertDialogDescription>
							Delete "{deleteTrack ? getTrackName(deleteTrack) : ""}"? This will
							remove the track and all associated analysis data.
						</AlertDialogDescription>
					</AlertDialogHeader>
					<AlertDialogFooter>
						<AlertDialogCancel>Cancel</AlertDialogCancel>
						<AlertDialogAction
							onClick={async () => {
								if (!deleteTrack) return;
								try {
									await invoke<void>("delete_track", {
										trackId: deleteTrack.id,
									});
									if (activeTrackId === deleteTrack.id) {
										useTrackEditorStore.getState().resetTrack();
									}
									await Promise.all([refresh(), refreshBrowser()]);
								} catch (err) {
									console.error("Failed to delete track:", err);
								}
							}}
						>
							Delete
						</AlertDialogAction>
					</AlertDialogFooter>
				</AlertDialogContent>
			</AlertDialog>
		</div>
	);
}
