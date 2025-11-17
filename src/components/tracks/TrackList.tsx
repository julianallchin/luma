import { invoke } from "@tauri-apps/api/core";
import { ask, open } from "@tauri-apps/plugin-dialog";
import { useEffect, useState } from "react";

import type { TrackSummary } from "@/bindings/schema";
import { Button } from "@/components/ui/button";
import { useTracksStore } from "@/useTracksStore";

const formatDuration = (seconds: number | null | undefined) => {
	if (seconds == null || Number.isNaN(seconds)) return null;
	const total = Math.max(0, seconds);
	const minutes = Math.floor(total / 60);
	const secs = Math.floor(total % 60)
		.toString()
		.padStart(2, "0");
	return `${minutes}:${secs}`;
};

export function TrackList() {
	const { tracks, loading, error: storeError, refresh } = useTracksStore();
	const [importing, setImporting] = useState(false);
	const [wiping, setWiping] = useState(false);
	const [error, setError] = useState<string | null>(null);
	const displayError = error ?? storeError;

	useEffect(() => {
		refresh().catch((err) => {
			console.error("Failed to load tracks", err);
		});
	}, [refresh]);

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

	const hasTracks = tracks.length > 0;

	return (
		<div className="flex h-full min-h-0 flex-col rounded-lg border border-border bg-card">
			<div className="flex items-center justify-between border-b border-border p-4">
				<div>
					<h2 className="text-lg font-semibold">Tracks</h2>
					<p className="text-xs text-muted-foreground">
						All imported audio available to your graphs.
					</p>
				</div>
				<div className="flex gap-2">
					<Button
						variant="destructive"
						onClick={handleWipe}
						disabled={wiping || loading}
					>
						{wiping ? "Wiping…" : "Wipe Track DB"}
					</Button>
					<Button onClick={handleImport} disabled={importing}>
						{importing ? "Importing…" : "Import Track"}
					</Button>
				</div>
			</div>
			{displayError && (
				<div className="border-b border-destructive bg-destructive/10 p-3 text-xs text-destructive">
					{displayError}
				</div>
			)}
			<div className="flex-1 overflow-y-auto p-4">
				{loading ? (
					<p className="text-center text-sm text-muted-foreground">
						Loading tracks…
					</p>
				) : hasTracks ? (
					<ul className="flex flex-col gap-3">
						{tracks.map((track) => (
							<li
								key={track.id}
								className="flex flex-col gap-2 rounded-lg border border-border bg-background/80 p-3 shadow-sm"
							>
								<div className="flex items-center gap-3">
									<div className="relative h-16 w-16 overflow-hidden rounded-md bg-muted">
										{track.albumArtData ? (
											<img
												src={track.albumArtData}
												alt={track.title ?? "Album art"}
												className="h-full w-full object-cover"
											/>
										) : (
											<div className="flex h-full w-full items-center justify-center text-xs uppercase text-muted-foreground">
												No Art
											</div>
										)}
									</div>
									<div className="flex-1 text-left text-sm">
										<p className="font-semibold">
											{track.title ?? "Untitled track"}
										</p>
										<p className="text-xs text-muted-foreground">
											{[track.artist, track.album]
												.filter(Boolean)
												.join(" • ") || "Unknown artist"}
										</p>
									</div>
								</div>
								<div className="grid gap-1 text-xs text-muted-foreground md:grid-cols-2">
									<p>
										Track {track.trackNumber ?? "—"} · Disc{" "}
										{track.discNumber ?? "—"}
									</p>
									<p>
										Duration:{" "}
										{formatDuration(track.durationSeconds) ?? "Unknown"}
									</p>
								</div>
								<p className="text-xs text-muted-foreground line-clamp-2">
									{track.filePath}
								</p>
							</li>
						))}
					</ul>
				) : (
					<div className="flex h-full flex-col items-center justify-center text-sm text-muted-foreground">
						No tracks imported yet. Use the import button to add one.
					</div>
				)}
			</div>
		</div>
	);
}
