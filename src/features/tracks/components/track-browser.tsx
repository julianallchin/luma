import { useVirtualizer } from "@tanstack/react-virtual";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import {
	Pencil,
	RefreshCw,
	RotateCcw,
	Search,
	Sparkles,
	Trash2,
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
import { autoLightTracks } from "@/features/track-editor/agent/auto-light";
import { getAgentApiKey } from "@/features/track-editor/agent/openrouter-key";
import { useReviewStatusStore } from "@/features/track-editor/agent/use-review-status-store";
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
import { Checkbox } from "@/shared/components/ui/checkbox";
import {
	ContextMenu,
	ContextMenuContent,
	ContextMenuItem,
	ContextMenuTrigger,
} from "@/shared/components/ui/context-menu";
import { Dropdown } from "@/shared/components/ui/dropdown";
import { Input } from "@/shared/components/ui/input";
import { Toggle } from "@/shared/components/ui/toggle";
import { cn } from "@/shared/lib/utils";
import { useTracksStore } from "../stores/use-tracks-store";
import { EditMetadataDialog } from "./edit-metadata-dialog";
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

// Module-level so it persists across `refreshBrowser` calls. The browser
// keeps the decoded image in its own cache too, but tracking which paths
// we've already kicked off saves us re-issuing every `convertFileSrc`
// fetch when a single track's analysis-progress tick triggers a list
// refresh.
const preloadedArtPaths = new Set<string>();

export function TrackBrowser() {
	const browserTracks = useTracksStore((s) => s.browserTracks);
	const browserLoading = useTracksStore((s) => s.browserLoading);
	const searchQuery = useTracksStore((s) => s.searchQuery);
	const setSearchQuery = useTracksStore((s) => s.setSearchQuery);
	const refreshBrowser = useTracksStore((s) => s.refreshBrowser);
	const refresh = useTracksStore((s) => s.refresh);
	const activeTrackId = useTrackEditorStore((s) => s.trackId);
	const currentVenueId = useAppViewStore((s) => s.currentVenue?.id ?? null);
	const currentVenueName = useAppViewStore((s) => s.currentVenue?.name ?? null);
	const currentUserId = useAuthStore((s) => s.user?.id ?? null);
	const reviewFlags = useReviewStatusStore((s) => s.flagged);

	const [importing, setImporting] = useState(false);
	const [scorePickerTrack, setScorePickerTrack] =
		useState<TrackBrowserRow | null>(null);
	const [djImportOpen, setDjImportOpen] = useState(false);
	const [deleteTrack, setDeleteTrack] = useState<TrackBrowserRow | null>(null);
	const [editMetadataTrack, setEditMetadataTrack] =
		useState<TrackBrowserRow | null>(null);
	const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
	const [lastSelectedIdx, setLastSelectedIdx] = useState<number | null>(null);
	const [deleteMultiConfirm, setDeleteMultiConfirm] = useState(false);
	// Files chosen from the picker, awaiting the user's "add to venue?" choice.
	// Only set when currentVenueId is non-null — otherwise import runs immediately.
	const [pendingImport, setPendingImport] = useState<string[] | null>(null);
	const openForSource = useDjImportStore((s) => s.openForSource);
	const [sourceFilter, setSourceFilter] = useState<"all" | "mine">("mine");
	// "In venue" filter is a separate axis from ownership — composes with
	// `sourceFilter`. Default on whenever there's a venue context, so users
	// land in the setlist view when they open a venue. When on, imports
	// auto-create empty score rows in this venue.
	const [inVenue, setInVenue] = useState<boolean>(true);
	const searchInputRef = useRef<HTMLInputElement>(null);
	const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);
	const scrollContainerRef = useRef<HTMLDivElement>(null);

	// Display name cache for other users' tracks
	const [displayNames, setDisplayNames] = useState<Record<string, string>>({});

	// Full load on mount
	useEffect(() => {
		refresh();
	}, [refresh]);

	// Reload browser tracks on mount and when venue changes
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
		const missing = otherUids.filter((uid) => !(uid in displayNames));
		if (missing.length === 0) return;
		invoke<Record<string, string>>("get_display_names", { uids: missing })
			.then((names) => setDisplayNames((prev) => ({ ...prev, ...names })))
			.catch(() => {});
	}, [browserTracks, currentUserId]); // displayNames deliberately omitted

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

	// Keep filteredTracks accessible to a stable keydown handler without
	// re-binding the listener on every render.
	const filteredTracksRef = useRef<TrackBrowserRow[]>([]);

	// Keyboard: Escape clears selection, Cmd/Ctrl+A selects all
	useEffect(() => {
		const onKey = (e: KeyboardEvent) => {
			const target = e.target as HTMLElement | null;
			const inEditable =
				!!target &&
				(target.tagName === "INPUT" ||
					target.tagName === "TEXTAREA" ||
					target.isContentEditable);
			if (e.key === "Escape") {
				setSelectedIds(new Set());
				setLastSelectedIdx(null);
			} else if (e.key === "a" && (e.metaKey || e.ctrlKey) && !inEditable) {
				e.preventDefault();
				setSelectedIds(new Set(filteredTracksRef.current.map((t) => t.id)));
			}
		};
		window.addEventListener("keydown", onKey);
		return () => window.removeEventListener("keydown", onKey);
	}, []);

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
		if (inVenue && currentVenueId) {
			result = result.filter((t) => t.venueAnnotationCount > 0);
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
	}, [
		browserTracks,
		searchQuery,
		sourceFilter,
		inVenue,
		currentVenueId,
		currentUserId,
	]);

	// Mirror to a ref so the global keydown handler can read it without
	// re-binding the listener.
	filteredTracksRef.current = filteredTracks;

	// Preload every album art into the browser image cache as soon as we
	// know the track list. Decoding happens off the main thread, so by the
	// time a row scrolls into view its <img> renders from cache instead of
	// kicking off a fresh fetch+decode (which is what causes the blank
	// 1-frame flash during fast scroll).
	useEffect(() => {
		for (const t of browserTracks) {
			if (!t.albumArtPath || preloadedArtPaths.has(t.albumArtPath)) continue;
			preloadedArtPaths.add(t.albumArtPath);
			const img = new Image();
			img.decoding = "async";
			img.src = convertFileSrc(t.albumArtPath);
		}
	}, [browserTracks]);

	// Cheap up-front: nothing selected ⇒ neither, all selected ⇒ trivially
	// derivable by size. Falls back to a single `.some(...)` only when the
	// selection is partial — avoiding two O(N) scans of `filteredTracks` per
	// render (used to run on every scroll-frame render).
	const { allSelected, someSelected } = useMemo(() => {
		if (selectedIds.size === 0 || filteredTracks.length === 0) {
			return { allSelected: false, someSelected: false };
		}
		const selectedInView = filteredTracks.reduce(
			(n, t) => (selectedIds.has(t.id) ? n + 1 : n),
			0,
		);
		return {
			allSelected: selectedInView === filteredTracks.length,
			someSelected:
				selectedInView > 0 && selectedInView < filteredTracks.length,
		};
	}, [filteredTracks, selectedIds]);

	const toggleSelect = (track: TrackBrowserRow, idx: number) => {
		setSelectedIds((prev) => {
			const next = new Set(prev);
			if (next.has(track.id)) next.delete(track.id);
			else next.add(track.id);
			return next;
		});
		setLastSelectedIdx(idx);
	};

	const virtualizer = useVirtualizer({
		count: filteredTracks.length,
		getScrollElement: () => scrollContainerRef.current,
		estimateSize: () => 32,
		overscan: 40,
	});

	const handleImport = async () => {
		const selection = await open({
			multiple: true,
			directory: false,
			title: "Select tracks to import",
		});
		if (!selection) return;
		const files = Array.isArray(selection) ? selection : [selection];
		if (files.length === 0) return;

		// In a venue: defer to the confirm dialog so the user picks scope.
		// Otherwise import straight to the library.
		if (currentVenueId && currentUserId) {
			setPendingImport(files);
			return;
		}
		await runImport(files, false);
	};

	const runImport = async (files: string[], addToVenue: boolean) => {
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

			// Empty score per imported track binds them to this venue's setlist.
			// Driven by the post-selection dialog's choice, not the filter toggle.
			if (addToVenue && currentVenueId && currentUserId) {
				await Promise.all(
					imported.map((t) =>
						invoke("create_score", {
							requestId: crypto.randomUUID(),
							trackId: t.id,
							venueId: currentVenueId,
						}).catch(() => {}),
					),
				);
			}

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

	// Optimistically remove tracks from store so the UI updates instantly
	const removeFromStore = (ids: Set<string>) => {
		useTracksStore.setState((s) => ({
			browserTracks: s.browserTracks.filter((t) => !ids.has(t.id)),
			tracks: s.tracks.filter((t) => !ids.has(t.id)),
		}));
	};

	const handleRowClick = (
		track: TrackBrowserRow,
		idx: number,
		e: React.MouseEvent,
	) => {
		if (e.metaKey || e.ctrlKey) {
			e.preventDefault();
			setSelectedIds((prev) => {
				const next = new Set(prev);
				if (next.has(track.id)) next.delete(track.id);
				else next.add(track.id);
				return next;
			});
			setLastSelectedIdx(idx);
		} else if (e.shiftKey && lastSelectedIdx !== null) {
			e.preventDefault();
			const start = Math.min(lastSelectedIdx, idx);
			const end = Math.max(lastSelectedIdx, idx);
			const rangeIds = filteredTracks.slice(start, end + 1).map((t) => t.id);
			setSelectedIds((prev) => new Set([...prev, ...rangeIds]));
		} else if (selectedIds.size > 0) {
			// Selection mode active — click selects single, or clears if clicking the only selected
			if (selectedIds.size === 1 && selectedIds.has(track.id)) {
				setSelectedIds(new Set());
				setLastSelectedIdx(null);
			} else {
				setSelectedIds(new Set([track.id]));
				setLastSelectedIdx(idx);
			}
		} else {
			handleTrackSelect(track);
		}
	};

	const handleSingleDeleteConfirm = async () => {
		if (!deleteTrack) return;
		const id = deleteTrack.id;
		const ids = new Set([id]);
		removeFromStore(ids);
		if (activeTrackId === id) useTrackEditorStore.getState().resetTrack();
		setDeleteTrack(null);
		try {
			await invoke<void>("delete_track", { trackId: id });
		} catch (err) {
			console.error("Failed to delete track:", err);
		}
		void Promise.all([refresh(), refreshBrowser()]);
	};

	const handleBulkDelete = async () => {
		const ids = new Set(selectedIds);
		removeFromStore(ids);
		if (activeTrackId && ids.has(activeTrackId)) {
			useTrackEditorStore.getState().resetTrack();
		}
		setSelectedIds(new Set());
		setLastSelectedIdx(null);
		setDeleteMultiConfirm(false);
		try {
			await Promise.all(
				[...ids].map((id) => invoke<void>("delete_track", { trackId: id })),
			);
		} catch (err) {
			console.error("Failed to delete tracks:", err);
		}
		void Promise.all([refresh(), refreshBrowser()]);
	};

	const handleBulkReprocess = () => {
		for (const id of selectedIds) {
			invoke("reprocess_track", { trackId: id }).catch((err) =>
				console.error("Failed to reprocess track:", err),
			);
		}
		setSelectedIds(new Set());
		setLastSelectedIdx(null);
	};

	const handleAutoLight = () => {
		if (!currentVenueId || !currentUserId) {
			toast.error("Open a venue to auto-light tracks.");
			return;
		}
		if (!getAgentApiKey()) {
			toast.error("Add your AI provider API key in Luma before auto-lighting.");
			return;
		}
		const picked = filteredTracks.filter((t) => selectedIds.has(t.id));
		if (picked.length === 0) return;
		setSelectedIds(new Set());
		setLastSelectedIdx(null);
		void autoLightTracks({
			tracks: picked,
			venueId: currentVenueId,
			venueName: currentVenueName,
			userId: currentUserId,
		});
	};

	return (
		<div className="flex flex-col h-full bg-background">
			{/* Header */}
			<div className="flex items-center gap-1 p-1 border-b border-trim">
				<div className="relative flex-1">
					<Search className="absolute left-2 top-1/2 -translate-y-1/2 h-3 w-3 text-muted-foreground pointer-events-none" />
					<Input
						ref={searchInputRef}
						type="text"
						placeholder="Search tracks..."
						defaultValue={searchQuery}
						onChange={(e) => handleSearchChange(e.target.value)}
						className="pl-7"
					/>
				</div>
				<div className="flex items-center gap-1">
					{/* Ownership filter — mutually exclusive, always one pressed. */}
					<div className="flex">
						<Toggle
							pressed={sourceFilter === "mine"}
							onClick={() => setSourceFilter("mine")}
						>
							mine
						</Toggle>
						<Toggle
							pressed={sourceFilter === "all"}
							onClick={() => setSourceFilter("all")}
							className="-ml-px"
						>
							all
						</Toggle>
					</div>
					{currentVenueId && (
						<Toggle pressed={inVenue} onClick={() => setInVenue((v) => !v)}>
							in venue
						</Toggle>
					)}
				</div>
				<Dropdown
					label="Import"
					disabled={importing}
					items={[
						{ label: "Upload Files", onClick: handleImport },
						{
							label: "Engine DJ",
							onClick: () => {
								openForSource(engineDjAdapter);
								setDjImportOpen(true);
							},
						},
						{
							label: "Rekordbox",
							onClick: () => {
								openForSource(rekordboxAdapter);
								setDjImportOpen(true);
							},
						},
					]}
				/>
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
			<div className="grid grid-cols-[28px_56px_1fr_1fr_70px_60px_60px_70px] gap-2 px-4 py-2 text-[10px] font-medium text-muted-foreground uppercase select-none border-b border-border/30">
				<div className="flex items-center justify-center">
					<Checkbox
						checked={someSelected ? "indeterminate" : allSelected}
						onCheckedChange={(checked) => {
							if (checked) {
								setSelectedIds(new Set(filteredTracks.map((t) => t.id)));
							} else {
								setSelectedIds(new Set());
								setLastSelectedIdx(null);
							}
						}}
						className="transition-opacity"
					/>
				</div>
				<div />
				<div>Title</div>
				<div>Artist</div>
				<div className="text-right">BPM</div>
				<div className="text-right">Time</div>
				<div className="text-center">Status</div>
				<div className="text-right">Added By</div>
			</div>

			{/* Track rows */}
			<div ref={scrollContainerRef} className="flex-1 overflow-y-auto">
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
					<div
						style={{ height: virtualizer.getTotalSize(), position: "relative" }}
					>
						{virtualizer.getVirtualItems().map((virtualItem) => {
							const track = filteredTracks[virtualItem.index];
							const isOwned = !track.uid || track.uid === currentUserId;
							const isSelected = selectedIds.has(track.id);

							const stripeBg =
								virtualItem.index % 2 === 0 ? "bg-card" : "bg-stripe";
							const trackButton = (
								// biome-ignore lint/a11y/useKeyWithClickEvents: desktop app
								// biome-ignore lint/a11y/noStaticElementInteractions: desktop app
								<div
									onClick={(e) => handleRowClick(track, virtualItem.index, e)}
									className={cn(
										"group w-full grid grid-cols-[28px_56px_1fr_1fr_70px_60px_60px_70px] gap-2 px-4 items-center text-left transition-colors duration-150 hover:duration-0 cursor-default",
										isSelected
											? "bg-primary/10"
											: activeTrackId === track.id
												? "bg-hover"
												: cn(stripeBg, "hover:bg-hover"),
									)}
								>
									{/* Checkbox */}
									{/* biome-ignore lint/a11y/noStaticElementInteractions: stopPropagation wrapper */}
									{/* biome-ignore lint/a11y/useKeyWithClickEvents: stopPropagation wrapper */}
									<div
										className="flex items-center justify-center"
										onClick={(e) => {
											e.stopPropagation();
											toggleSelect(track, virtualItem.index);
										}}
									>
										<Checkbox checked={isSelected} />
									</div>

									{/* Album art */}
									<div className="relative h-8 w-14 overflow-hidden bg-muted/50 flex-shrink-0">
										{track.albumArtPath ? (
											<img
												src={convertFileSrc(track.albumArtPath)}
												alt=""
												loading="lazy"
												decoding="async"
												className="h-full w-full object-cover"
											/>
										) : (
											<div className="w-full h-full flex items-center justify-center bg-black/20 text-[7px] text-muted-foreground uppercase tracking-tighter">
												No Art
											</div>
										)}
									</div>

									{/* Title */}
									<div className="text-xs font-medium text-foreground/90 truncate flex items-center gap-1.5">
										{currentVenueId && track.durationSeconds
											? (() => {
													const needsReview = Boolean(
														reviewFlags[`${track.id}:${currentVenueId}`],
													);
													const pct = Math.min(
														1,
														track.venueAnnotationCoverageSeconds /
															track.durationSeconds,
													);
													const color = needsReview
														? "bg-sky-500"
														: pct === 0
															? "bg-rose-500"
															: pct >= 0.7
																? "bg-emerald-500"
																: "bg-amber-500";
													const tip = needsReview
														? "Auto-lit — open to review"
														: `${Math.round(pct * 100)}% covered by annotations (${track.venueAnnotationCount})`;
													return (
														<span
															className={cn(
																"w-1.5 h-1.5 rounded-full shrink-0",
																color,
															)}
															title={tip}
														/>
													);
												})()
											: null}
										{getTrackName(track)}
									</div>

									{/* Artist */}
									<div className="text-xs text-foreground/90 truncate">
										{track.artist || "Unknown artist"}
									</div>

									{/* BPM */}
									<div className="text-xs text-foreground/90 text-right font-mono">
										{track.bpm ? track.bpm.toFixed(1) : "--"}
									</div>

									{/* Duration */}
									<div className="text-xs text-foreground/90 text-right font-mono">
										{formatDuration(track.durationSeconds)}
									</div>

									{/* Preprocessing status */}
									<PreprocessingStatus track={track} />

									{/* Added by */}
									<div className="text-xs text-foreground/90 text-right">
										{track.uid && track.uid !== currentUserId
											? (displayNames[track.uid] ?? "shared")
											: "you"}
									</div>
								</div>
							);

							const row = isOwned ? (
								<ContextMenu>
									<ContextMenuTrigger asChild>{trackButton}</ContextMenuTrigger>
									<ContextMenuContent>
										<ContextMenuItem
											onClick={() => setEditMetadataTrack(track)}
										>
											<Pencil />
											Edit Metadata
										</ContextMenuItem>
										<ContextMenuItem
											onClick={() => {
												invoke("reprocess_track", {
													trackId: track.id,
												}).catch((err) =>
													console.error("Failed to reprocess track:", err),
												);
											}}
										>
											<RefreshCw />
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
											<RotateCcw />
											Reprocess Waveform
										</ContextMenuItem>
										<ContextMenuItem
											variant="destructive"
											onClick={() => setDeleteTrack(track)}
										>
											<Trash2 />
											Delete
										</ContextMenuItem>
									</ContextMenuContent>
								</ContextMenu>
							) : (
								trackButton
							);

							return (
								<div
									key={virtualItem.key}
									style={{
										position: "absolute",
										top: 0,
										left: 0,
										width: "100%",
										height: `${virtualItem.size}px`,
										transform: `translateY(${virtualItem.start}px)`,
									}}
								>
									{row}
								</div>
							);
						})}
					</div>
				)}
			</div>

			{/* Bulk action bar — shown when tracks are selected */}
			{selectedIds.size > 0 && (
				<div className="flex items-center gap-2 px-4 py-2 border-t border-border/50 bg-muted/20">
					<span className="text-xs text-muted-foreground flex-1">
						{selectedIds.size} selected
					</span>
					{currentVenueId && (
						<Button className="h-7 text-xs" onClick={handleAutoLight}>
							<Sparkles className="size-3" />
							Auto-light
						</Button>
					)}
					<Button className="h-7 text-xs" onClick={handleBulkReprocess}>
						<RefreshCw className="size-3" />
						Reprocess
					</Button>
					<Button
						className="h-7 text-xs"
						onClick={() => setDeleteMultiConfirm(true)}
					>
						<Trash2 className="size-3" />
						Delete
					</Button>
					<Button
						className="h-7 text-xs"
						onClick={() => {
							setSelectedIds(new Set());
							setLastSelectedIdx(null);
						}}
					>
						Clear
					</Button>
				</div>
			)}

			{/* Footer */}
			<div className="px-4 py-2 border-t border-border/30 text-[10px] text-muted-foreground">
				{filteredTracks.length} track{filteredTracks.length !== 1 ? "s" : ""}
			</div>

			<EditMetadataDialog
				track={editMetadataTrack}
				open={editMetadataTrack !== null}
				onOpenChange={(open) => {
					if (!open) setEditMetadataTrack(null);
				}}
			/>

			{/* Post-selection import scope confirmation (only when in a venue) */}
			<AlertDialog
				open={pendingImport !== null}
				onOpenChange={(open) => {
					if (!open) setPendingImport(null);
				}}
			>
				<AlertDialogContent>
					<AlertDialogHeader>
						<AlertDialogTitle>
							Add to {currentVenueName ?? "venue"}?
						</AlertDialogTitle>
						<AlertDialogDescription>
							{pendingImport?.length ?? 0} track
							{(pendingImport?.length ?? 0) !== 1 ? "s" : ""} will be imported
							to your library. Also add{" "}
							{(pendingImport?.length ?? 0) !== 1 ? "them" : "it"} to{" "}
							{currentVenueName ?? "this venue"}'s setlist?
						</AlertDialogDescription>
					</AlertDialogHeader>
					<AlertDialogFooter>
						<AlertDialogCancel
							onClick={() => {
								const files = pendingImport;
								setPendingImport(null);
								if (files) void runImport(files, false);
							}}
						>
							Library only
						</AlertDialogCancel>
						<AlertDialogAction
							onClick={() => {
								const files = pendingImport;
								setPendingImport(null);
								if (files) void runImport(files, true);
							}}
						>
							Add to {currentVenueName ?? "venue"}
						</AlertDialogAction>
					</AlertDialogFooter>
				</AlertDialogContent>
			</AlertDialog>

			{/* Single track delete confirmation */}
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
						<AlertDialogAction onClick={handleSingleDeleteConfirm}>
							Delete
						</AlertDialogAction>
					</AlertDialogFooter>
				</AlertDialogContent>
			</AlertDialog>

			{/* Multi-track delete confirmation */}
			<AlertDialog
				open={deleteMultiConfirm}
				onOpenChange={(open) => {
					if (!open) setDeleteMultiConfirm(false);
				}}
			>
				<AlertDialogContent>
					<AlertDialogHeader>
						<AlertDialogTitle>
							Delete {selectedIds.size} tracks
						</AlertDialogTitle>
						<AlertDialogDescription>
							Delete {selectedIds.size} track
							{selectedIds.size !== 1 ? "s" : ""}? This will remove them and all
							associated analysis data.
						</AlertDialogDescription>
					</AlertDialogHeader>
					<AlertDialogFooter>
						<AlertDialogCancel>Cancel</AlertDialogCancel>
						<AlertDialogAction onClick={handleBulkDelete}>
							Delete {selectedIds.size}
						</AlertDialogAction>
					</AlertDialogFooter>
				</AlertDialogContent>
			</AlertDialog>
		</div>
	);
}
