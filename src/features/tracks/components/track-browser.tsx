import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { ask, open } from "@tauri-apps/plugin-dialog";
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
import type { TrackBrowserRow, TrackSummary } from "@/bindings/schema";
import { EngineDjBrowser } from "@/features/engine-dj/components/engine-dj-browser";
import type { TrackWaveform } from "@/features/track-editor/stores/use-track-editor-store";
import { useTrackEditorStore } from "@/features/track-editor/stores/use-track-editor-store";
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
	const loadTrack = useTrackEditorStore((s) => s.loadTrack);
	const loadPatterns = useTrackEditorStore((s) => s.loadPatterns);
	const activeTrackId = useTrackEditorStore((s) => s.trackId);

	const [importing, setImporting] = useState(false);
	const [engineDjOpen, setEngineDjOpen] = useState(false);
	const [sourceFilter, setSourceFilter] = useState<
		"all" | "engine_dj" | "file"
	>("all");
	const searchInputRef = useRef<HTMLInputElement>(null);
	const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);

	useEffect(() => {
		refreshBrowser();
	}, [refreshBrowser]);

	// Listen for track analysis completion and refresh browser (debounced)
	useEffect(() => {
		let timeout: ReturnType<typeof setTimeout> | null = null;
		let unsub: (() => void) | null = null;
		let cancelled = false;

		listen<number>("track-status-changed", () => {
			if (timeout) clearTimeout(timeout);
			timeout = setTimeout(() => {
				refreshBrowser();
			}, 500);
		}).then((unlisten) => {
			if (cancelled) unlisten();
			else unsub = unlisten;
		});

		return () => {
			cancelled = true;
			if (unsub) unsub();
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
		if (sourceFilter !== "all") {
			result = result.filter((t) => t.sourceType === sourceFilter);
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
	}, [browserTracks, searchQuery, sourceFilter]);

	const handleImport = async () => {
		const selection = await open({
			multiple: false,
			directory: false,
			title: "Select a track to import",
		});
		if (typeof selection !== "string") return;

		setImporting(true);
		try {
			await invoke<TrackSummary>("import_track", { filePath: selection });
			await Promise.all([refresh(), refreshBrowser()]);
		} catch (err) {
			console.error("Failed to import track:", err);
		} finally {
			setImporting(false);
		}
	};

	const handleTrackSelect = (track: TrackBrowserRow) => {
		const trackName = getTrackName(track);
		void loadTrack(track.id, trackName);
		void loadPatterns();
	};

	const handleEngineDjClose = (open: boolean) => {
		setEngineDjOpen(open);
		if (!open) {
			void refreshBrowser();
			void refresh();
		}
	};

	const sourceLabel = (sourceType: string | null) => {
		if (sourceType === "engine_dj") return "Engine DJ";
		if (sourceType === "file") return "File";
		return sourceType ?? "Unknown";
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
							{ id: "all", label: "All" },
							{ id: "engine_dj", label: "Engine DJ" },
							{ id: "file", label: "File" },
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
							Upload File
						</DropdownMenuItem>
						<DropdownMenuItem onClick={() => setEngineDjOpen(true)}>
							<Disc3 className="size-4" />
							Import from Engine DJ
						</DropdownMenuItem>
					</DropdownMenuContent>
				</DropdownMenu>
				<EngineDjBrowser
					open={engineDjOpen}
					onOpenChange={handleEngineDjClose}
				/>
			</div>

			{/* Column headers */}
			<div className="grid grid-cols-[40px_1fr_1fr_70px_60px_60px_80px] gap-2 px-4 py-2 text-[10px] font-medium text-muted-foreground uppercase select-none border-b border-border/30">
				<div />
				<div>Title</div>
				<div>Artist</div>
				<div className="text-right">BPM</div>
				<div className="text-right">Time</div>
				<div className="text-center">Status</div>
				<div className="text-right">Source</div>
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
					filteredTracks.map((track) => (
						<ContextMenu key={track.id}>
							<ContextMenuTrigger asChild>
								<button
									type="button"
									onClick={() => handleTrackSelect(track)}
									className={cn(
										"w-full grid grid-cols-[40px_1fr_1fr_70px_60px_60px_80px] gap-2 px-4 py-1.5 items-center text-left transition-colors duration-150 hover:duration-0",
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
									<div className="text-xs font-medium text-foreground/90 truncate">
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

									{/* Status dots */}
									<div className="flex items-center justify-center gap-1">
										<div
											className={cn(
												"w-2 h-2 rounded-full",
												track.hasBeats
													? "bg-emerald-500"
													: "bg-muted-foreground/20",
											)}
											title={track.hasBeats ? "Beats analyzed" : "No beats"}
										/>
										<div
											className={cn(
												"w-2 h-2 rounded-full",
												track.hasStems
													? "bg-emerald-500"
													: "bg-muted-foreground/20",
											)}
											title={track.hasStems ? "Stems separated" : "No stems"}
										/>
										<div
											className={cn(
												"w-2 h-2 rounded-full",
												track.hasRoots
													? "bg-emerald-500"
													: "bg-muted-foreground/20",
											)}
											title={track.hasRoots ? "Chords analyzed" : "No chords"}
										/>
									</div>

									{/* Source badge */}
									<div className="flex justify-end">
										<span className="text-[10px] px-1.5 py-0.5 bg-muted text-muted-foreground rounded">
											{sourceLabel(track.sourceType)}
										</span>
									</div>
								</button>
							</ContextMenuTrigger>
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
									onClick={async () => {
										const trackName = getTrackName(track);
										const confirmed = await ask(
											`Delete "${trackName}"? This will remove the track and all associated analysis data.`,
											{
												title: "Delete track",
												kind: "warning",
											},
										);
										if (!confirmed) return;
										try {
											await invoke<void>("delete_track", {
												trackId: track.id,
											});
											if (activeTrackId === track.id) {
												useTrackEditorStore.getState().resetTrack();
											}
											await Promise.all([refresh(), refreshBrowser()]);
										} catch (err) {
											console.error("Failed to delete track:", err);
										}
									}}
								>
									<Trash2 className="size-4" />
									Delete
								</ContextMenuItem>
							</ContextMenuContent>
						</ContextMenu>
					))
				)}
			</div>

			{/* Footer */}
			<div className="px-4 py-2 border-t border-border/30 text-[10px] text-muted-foreground">
				{filteredTracks.length} track{filteredTracks.length !== 1 ? "s" : ""}
			</div>
		</div>
	);
}
