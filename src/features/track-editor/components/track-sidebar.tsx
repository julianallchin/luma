import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { ChevronDown, ChevronLeft, Disc3, Upload } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type { TrackSummary } from "@/bindings/schema";
import { EngineDjBrowser } from "@/features/engine-dj/components/engine-dj-browser";
import { CreatePatternDialog } from "@/features/patterns/components/create-pattern-dialog";
import { useTracksStore } from "@/features/tracks/stores/use-tracks-store";
import { Button } from "@/shared/components/ui/button";
import {
	DropdownMenu,
	DropdownMenuContent,
	DropdownMenuItem,
	DropdownMenuTrigger,
} from "@/shared/components/ui/dropdown-menu";
import { cn } from "@/shared/lib/utils";
import { useTrackEditorStore } from "../stores/use-track-editor-store";
import { PatternRegistry } from "./pattern-registry";

const formatDuration = (seconds: number | null | undefined) => {
	if (seconds == null || Number.isNaN(seconds)) return "--:--";
	const total = Math.max(0, seconds);
	const minutes = Math.floor(total / 60);
	const secs = Math.floor(total % 60)
		.toString()
		.padStart(2, "0");
	return `${minutes}:${secs}`;
};

const getTrackName = (track: TrackSummary) =>
	track.title || track.filePath.split("/").pop() || "Untitled";

export function TrackSidebar() {
	const { tracks, loading, error, refresh } = useTracksStore();
	const loadTrack = useTrackEditorStore((s) => s.loadTrack);
	const activeTrackId = useTrackEditorStore((s) => s.trackId);
	const loadPatterns = useTrackEditorStore((s) => s.loadPatterns);
	const [page, setPage] = useState<"tracks" | "patterns">(
		activeTrackId !== null ? "patterns" : "tracks",
	);
	const [importing, setImporting] = useState(false);
	const [importError, setImportError] = useState<string | null>(null);
	const [engineDjOpen, setEngineDjOpen] = useState(false);
	const lastTrackIdRef = useRef<number | null>(activeTrackId);

	useEffect(() => {
		if (tracks.length === 0) {
			refresh().catch((err) => {
				console.error("Failed to load tracks", err);
			});
		}
	}, [refresh, tracks.length]);

	useEffect(() => {
		if (lastTrackIdRef.current === null && activeTrackId !== null) {
			setPage("patterns");
		}
		if (lastTrackIdRef.current !== null && activeTrackId === null) {
			setPage("tracks");
		}
		lastTrackIdRef.current = activeTrackId;
	}, [activeTrackId]);

	const handleImport = async () => {
		setImportError(null);
		const selection = await open({
			multiple: false,
			directory: false,
			title: "Select a track to import",
		});
		if (typeof selection !== "string") return;

		setImporting(true);
		try {
			await invoke<TrackSummary>("import_track", { filePath: selection });
			await refresh();
		} catch (err) {
			setImportError(err instanceof Error ? err.message : String(err));
		} finally {
			setImporting(false);
		}
	};

	const handleTrackSelect = (track: TrackSummary) => {
		const trackName = getTrackName(track);
		void loadTrack(track.id, trackName);
		setPage("patterns");
	};

	const displayError = importError ?? error;

	return (
		<div className="w-80 border-r border-border flex flex-col bg-background/50">
			<div className="flex-1 overflow-hidden">
				<div
					className={cn(
						"flex h-full transition-transform duration-300 ease-in-out",
						page === "patterns" ? "-translate-x-full" : "translate-x-0",
					)}
				>
					<div className="w-full shrink-0 flex flex-col">
						<div className="p-3 border-b border-border/50 flex items-center justify-between">
							<h2 className="text-xs font-semibold text-muted-foreground uppercase tracking-wide">
								Tracks
							</h2>
							<span className="text-[10px] text-muted-foreground">
								{tracks.length}
							</span>
						</div>

						{displayError && (
							<div className="bg-destructive/10 px-3 py-2 text-xs text-destructive border-b border-destructive/20 select-text">
								{displayError}
							</div>
						)}

						<div className="flex-1 overflow-y-auto p-2 space-y-1">
							{loading && tracks.length === 0 ? (
								<div className="p-2 text-xs text-muted-foreground">
									Loading tracks...
								</div>
							) : tracks.length === 0 ? (
								<div className="p-4 text-xs text-muted-foreground text-center">
									No tracks imported
								</div>
							) : (
								tracks.map((track) => (
									<button
										key={track.id}
										type="button"
										onClick={() => handleTrackSelect(track)}
										className={cn(
											"group w-full rounded-md px-2 py-2 text-left transition-colors",
											activeTrackId === track.id
												? "bg-muted"
												: "hover:bg-muted/50",
										)}
									>
										<div className="flex items-center justify-between gap-2">
											<div className="flex items-center gap-2 min-w-0">
												<div className="relative h-9 w-9 overflow-hidden rounded bg-muted/50 flex-shrink-0">
													{track.albumArtData ? (
														<img
															src={track.albumArtData}
															alt=""
															className="h-full w-full object-cover"
														/>
													) : (
														<div className="w-full h-full flex items-center justify-center bg-muted text-[8px] text-muted-foreground uppercase tracking-tighter">
															No Art
														</div>
													)}
												</div>
												<div className="min-w-0">
													<div className="text-xs font-medium text-foreground/90 truncate">
														{getTrackName(track)}
													</div>
													<div className="text-[10px] text-muted-foreground truncate">
														{track.artist || "Unknown artist"}
													</div>
												</div>
											</div>
											<div className="text-[10px] text-muted-foreground font-mono">
												{formatDuration(track.durationSeconds)}
											</div>
										</div>
									</button>
								))
							)}
						</div>

						<div className="p-3 border-t border-border/50">
							<DropdownMenu>
								<DropdownMenuTrigger asChild>
									<Button
										variant="outline"
										size="sm"
										className="w-full"
										disabled={importing}
									>
										Import
										<ChevronDown className="size-3 ml-1" />
									</Button>
								</DropdownMenuTrigger>
								<DropdownMenuContent align="center" className="w-56">
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
							<EngineDjBrowser open={engineDjOpen} onOpenChange={setEngineDjOpen} />
						</div>
					</div>

					<div className="w-full shrink-0 flex flex-col">
						<div className="p-3 border-b border-border/50 flex items-center justify-between gap-2">
							<div className="flex items-center gap-2">
								<button
									type="button"
									onClick={() => setPage("tracks")}
									className="text-muted-foreground hover:text-foreground transition-colors"
									aria-label="Back to tracks"
								>
									<ChevronLeft className="h-4 w-4" />
								</button>
								<h2 className="text-xs font-semibold text-muted-foreground uppercase tracking-wide">
									Patterns
								</h2>
							</div>
							<CreatePatternDialog
								trigger={
									<Button
										variant="outline"
										size="sm"
										className="h-7 px-2 text-xs"
									>
										Create
									</Button>
								}
								onCreated={() => loadPatterns()}
							/>
						</div>
						<div className="flex-1 overflow-y-auto">
							<PatternRegistry />
						</div>
					</div>
				</div>
			</div>
		</div>
	);
}
