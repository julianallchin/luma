import { invoke } from "@tauri-apps/api/core";
import { ask, open } from "@tauri-apps/plugin-dialog";
import { Trash2 } from "lucide-react";
import { useEffect, useState } from "react";
import { useNavigate } from "react-router-dom";

import type { TrackSummary } from "@/bindings/schema";
import { useTracksStore } from "@/features/tracks/stores/use-tracks-store";
import { Button } from "@/shared/components/ui/button";
import {
	ContextMenu,
	ContextMenuContent,
	ContextMenuItem,
	ContextMenuTrigger,
} from "@/shared/components/ui/context-menu";

const formatDuration = (seconds: number | null | undefined) => {
	if (seconds == null || Number.isNaN(seconds)) return "--:--";
	const total = Math.max(0, seconds);
	const minutes = Math.floor(total / 60);
	const secs = Math.floor(total % 60)
		.toString()
		.padStart(2, "0");
	return `${minutes}:${secs}`;
};

export function TrackList() {
	const { tracks, loading, error: storeError, refresh } = useTracksStore();
	const navigate = useNavigate();
	const [importing, setImporting] = useState(false);
	const [wiping, setWiping] = useState(false);
	const [error, setError] = useState<string | null>(null);
	const displayError = error ?? storeError;

	const handleTrackClick = (track: TrackSummary) => {
		const trackName =
			track.title || track.filePath.split("/").pop() || "Untitled";
		navigate(`/track/${track.id}`, { state: { trackName } });
	};

	useEffect(() => {
		// Only fetch if we have no tracks and aren't currently loading
		// This prevents re-fetching on tab switches if data exists
		if (tracks.length === 0) {
			refresh().catch((err) => {
				console.error("Failed to load tracks", err);
			});
		}
	}, [refresh, tracks.length]);

	const handleImport = async () => {
		setError(null);
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
			setError(err instanceof Error ? err.message : String(err));
		} finally {
			setImporting(false);
		}
	};

	const handleWipe = async () => {
		const confirmed = await ask(
			"Delete all imported tracks and cached analysis data?",
			{
				title: "Confirm wipe",
				kind: "warning",
			},
		);
		if (!confirmed) {
			return;
		}

		setError(null);
		setWiping(true);
		try {
			await invoke<void>("wipe_tracks");
			await refresh();
		} catch (err) {
			setError(err instanceof Error ? err.message : String(err));
		} finally {
			setWiping(false);
		}
	};

	const handleDeleteTrack = async (
		track: TrackSummary,
		e?: React.MouseEvent,
	) => {
		e?.stopPropagation();
		const trackName =
			track.title || track.filePath.split("/").pop() || "Untitled";
		const confirmed = await ask(
			`Delete "${trackName}"? This will remove the track and all associated analysis data.`,
			{
				title: "Delete track",
				kind: "warning",
			},
		);
		if (!confirmed) {
			return;
		}

		setError(null);
		try {
			await invoke<void>("delete_track", { trackId: track.id });
			await refresh();
		} catch (err) {
			setError(err instanceof Error ? err.message : String(err));
		}
	};

	if (loading) {
		return (
			<div className="p-8 text-xs text-muted-foreground">Loading tracks...</div>
		);
	}

	return (
		<div className="flex flex-col h-full">
			<div className="flex items-center justify-between p-2 border-b border-border/50 min-h-[40px]">
				<div className="text-xs text-muted-foreground px-2">
					{tracks.length} tracks
				</div>
				<div className="flex gap-2">
					<Button
						variant="ghost"
						size="sm"
						onClick={handleWipe}
						className="h-7 text-xs px-2 text-muted-foreground hover:text-destructive"
						disabled={wiping}
					>
						Wipe DB
					</Button>
					<Button
						variant="ghost"
						size="sm"
						onClick={handleImport}
						className="h-7 text-xs px-2"
						disabled={importing}
					>
						Import Track
					</Button>
				</div>
			</div>

			{displayError && (
				<div className="bg-destructive/10 p-2 text-xs text-destructive border-b border-destructive/20 select-text">
					{displayError}
				</div>
			)}

			<div className="grid grid-cols-[40px_40px_1fr_1fr_80px] gap-4 px-4 py-2 text-xs font-medium text-muted-foreground border-b border-border/50 select-none">
				<div>#</div>
				<div></div>
				<div>TITLE</div>
				<div>ARTIST</div>
				<div className="text-right">TIME</div>
			</div>

			<div className="flex-1 overflow-y-auto">
				{tracks.length === 0 ? (
					<div className="flex flex-col items-center justify-center h-32 text-xs text-muted-foreground">
						No tracks imported
					</div>
				) : (
					tracks.map((track, i) => (
						<ContextMenu key={track.id}>
							<ContextMenuTrigger asChild>
								<button
									type="button"
									onClick={() => handleTrackClick(track)}
									className="w-full grid grid-cols-[40px_40px_1fr_1fr_80px] gap-4 px-4 py-1.5 text-sm hover:bg-muted items-center group cursor-pointer text-left"
								>
									<div className="text-xs text-muted-foreground font-mono opacity-50 group-hover:opacity-100">
										{i + 1}
									</div>
									<div className="relative h-8 w-8 overflow-hidden rounded bg-muted/50">
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
									<div className="font-medium truncate text-foreground/90">
										{track.title || "Untitled"}
									</div>
									<div className="text-muted-foreground truncate text-xs">
										{track.artist || "Unknown"}
									</div>
									<div className="text-xs text-muted-foreground text-right font-mono opacity-70">
										{formatDuration(track.durationSeconds)}
									</div>
								</button>
							</ContextMenuTrigger>
							<ContextMenuContent>
								<ContextMenuItem
									variant="destructive"
									onClick={(e) => handleDeleteTrack(track, e)}
								>
									<Trash2 className="size-4" />
									Delete
								</ContextMenuItem>
							</ContextMenuContent>
						</ContextMenu>
					))
				)}
			</div>
		</div>
	);
}
