import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { Folder, Library, Loader2 } from "lucide-react";
import { useCallback, useRef, useState } from "react";
import { toast } from "sonner";
import type { TrackImportProgress } from "@/bindings/schema";
import { useTracksStore } from "@/features/tracks/stores/use-tracks-store";
import { Button } from "@/shared/components/ui/button";
import { Checkbox } from "@/shared/components/ui/checkbox";
import {
	Dialog,
	DialogContent,
	DialogHeader,
	DialogTitle,
} from "@/shared/components/ui/dialog";
import { Input } from "@/shared/components/ui/input";
import { cn } from "@/shared/lib/utils";
import {
	type DjPlaylist,
	useDjImportStore,
} from "../stores/use-dj-import-store";

const formatDuration = (seconds: number | null | undefined) => {
	if (seconds == null || Number.isNaN(seconds)) return "--:--";
	const total = Math.max(0, seconds);
	const minutes = Math.floor(total / 60);
	const secs = Math.floor(total % 60)
		.toString()
		.padStart(2, "0");
	return `${minutes}:${secs}`;
};

interface DjImportBrowserProps {
	open: boolean;
	onOpenChange: (open: boolean) => void;
}

export function DjImportBrowser({ open, onOpenChange }: DjImportBrowserProps) {
	const source = useDjImportStore((s) => s.source);
	const libraryInfo = useDjImportStore((s) => s.libraryInfo);
	const selectedKeys = useDjImportStore((s) => s.selectedKeys);
	const importing = useDjImportStore((s) => s.importing);
	const error = useDjImportStore((s) => s.error);
	const searchFn = useDjImportStore((s) => s.search);
	const importSelected = useDjImportStore((s) => s.importSelected);
	const reset = useDjImportStore((s) => s.reset);
	const refreshTracks = useTracksStore((s) => s.refresh);
	const refreshBrowser = useTracksStore((s) => s.refreshBrowser);
	const searchTimeout = useRef<ReturnType<typeof setTimeout> | null>(null);
	const [searchValue, setSearchValue] = useState("");

	const handleSearch = useCallback(
		(value: string) => {
			setSearchValue(value);
			if (searchTimeout.current) clearTimeout(searchTimeout.current);
			searchTimeout.current = setTimeout(() => {
				searchFn(value);
			}, 300);
		},
		[searchFn],
	);

	const handleImport = useCallback(async () => {
		const count = selectedKeys.size;
		let importId: string | null = null;
		let importedCount = 0;
		let unlisten: UnlistenFn | null = null;
		const pending: TrackImportProgress[] = [];
		const toastId = "bg-analysis";
		const showProgress = (progress: TrackImportProgress) => {
			if (progress.phase === "analyzing") {
				toast.loading(progress.step ?? "Analyzing tracks…", { id: toastId });
			} else if (progress.phase === "complete") {
				if (progress.error) {
					toast.error("Analysis completed with errors", {
						id: toastId,
						description: progress.error,
					});
				} else if (importedCount > 0) {
					toast.success("Analysis complete", { id: toastId });
				}
				unlisten?.();
				unlisten = null;
			}
		};
		unlisten = await listen<TrackImportProgress>(
			"track-import-state",
			(event) => {
				if (event.payload.source !== source?.name) return;
				if (importId === null) {
					pending.push(event.payload);
				} else if (event.payload.importId === importId) {
					showProgress(event.payload);
				}
			},
		);
		const result = await importSelected();
		if (result?.tracks.length) {
			await Promise.all([refreshTracks(), refreshBrowser()]);
		} else if (count > 0) {
			toast.error("Import failed");
		}
		if (result && result.failures.length > 0 && result.tracks.length > 0) {
			toast.warning(
				`Imported ${result.tracks.length}/${count} tracks; ${result.failures.length} failed`,
			);
		}
		onOpenChange(false);

		if (result) {
			importId = result.importId;
			importedCount = result.tracks.length;
			if (result.tracks.length > 0) {
				toast.loading(
					`Analyzing ${result.tracks.length} track${result.tracks.length !== 1 ? "s" : ""}…`,
					{ id: toastId },
				);
			} else {
				unlisten();
				unlisten = null;
			}
			for (const progress of pending) {
				if (progress.importId === importId) showProgress(progress);
			}
		} else {
			unlisten();
		}
	}, [
		importSelected,
		refreshTracks,
		refreshBrowser,
		selectedKeys.size,
		onOpenChange,
		source?.name,
	]);

	const handleClose = useCallback(
		(nextOpen: boolean) => {
			if (!nextOpen) {
				reset();
				setSearchValue("");
			}
			onOpenChange(nextOpen);
		},
		[onOpenChange, reset],
	);

	const label = source?.label ?? "DJ Library";

	return (
		<Dialog open={open} onOpenChange={handleClose}>
			<DialogContent className="sm:max-w-6xl h-[600px] flex flex-col p-0 gap-0 rounded-none">
				<DialogHeader className="px-4 py-3 border-b border-border/50 shrink-0">
					<DialogTitle className="text-sm font-medium">
						{libraryInfo
							? `${label} Library — ${libraryInfo.trackCount} tracks`
							: `Import from ${label}`}
					</DialogTitle>
				</DialogHeader>

				{error && (
					<div className="bg-destructive/10 px-4 py-2 text-xs text-destructive border-b border-destructive/20 shrink-0">
						{error}
					</div>
				)}

				{!libraryInfo ? (
					<div className="flex-1 flex items-center justify-center">
						<div className="flex items-center gap-2 text-sm text-muted-foreground">
							<Loader2 className="size-4 animate-spin" />
							Loading {label} library...
						</div>
					</div>
				) : (
					<>
						<div className="px-4 py-2 border-b border-border/50 shrink-0">
							<Input
								placeholder="Search tracks..."
								value={searchValue}
								onChange={(e) => handleSearch(e.target.value)}
								className="rounded-none text-xs"
							/>
						</div>
						<div className="flex flex-1 overflow-hidden">
							<Sidebar />
							<TrackList />
						</div>
						<ImportProgress />
						<div className="flex items-center justify-between px-4 py-3 border-t border-border/50 shrink-0">
							<div className="text-xs text-muted-foreground">
								{selectedKeys.size > 0
									? `${selectedKeys.size} tracks selected`
									: "Select tracks to import"}
							</div>
							<Button
								onClick={handleImport}
								disabled={selectedKeys.size === 0 || importing}
							>
								{importing
									? "Importing..."
									: `Import Selected (${selectedKeys.size})`}
							</Button>
						</div>
					</>
				)}
			</DialogContent>
		</Dialog>
	);
}

function Sidebar() {
	const playlists = useDjImportStore((s) => s.playlists);
	const activeView = useDjImportStore((s) => s.activeView);
	const activePlaylistKey = useDjImportStore((s) => s.activePlaylistKey);
	const selectPlaylist = useDjImportStore((s) => s.selectPlaylist);
	const libraryInfo = useDjImportStore((s) => s.libraryInfo);

	const topLevel = playlists.filter((p) => !p.parentKey);
	const children = (parentKey: string) =>
		playlists.filter((p) => p.parentKey === parentKey);

	return (
		<div className="w-56 border-r border-border/50 flex flex-col overflow-hidden">
			<div className="p-2 border-b border-border/50">
				<div className="text-[10px] font-medium text-muted-foreground uppercase tracking-wider px-2 py-1">
					Library
				</div>
				{libraryInfo && (
					<div className="text-[10px] text-muted-foreground/60 px-2">
						{libraryInfo.trackCount} tracks
					</div>
				)}
			</div>
			<div className="flex-1 overflow-y-auto p-1 space-y-0.5">
				<button
					type="button"
					onClick={() => selectPlaylist(null)}
					className={cn(
						"w-full flex items-center gap-2 px-2 py-1.5 text-xs text-left transition-colors duration-150 hover:duration-0",
						activeView === "all" && activePlaylistKey === null
							? "bg-muted text-foreground"
							: "text-muted-foreground hover:bg-muted/50 hover:text-foreground",
					)}
				>
					<Library className="size-3.5 shrink-0" />
					All Tracks
				</button>

				{topLevel.map((playlist) => (
					<PlaylistItem
						key={playlist.key}
						playlist={playlist}
						isActive={activePlaylistKey === playlist.key}
						onSelect={selectPlaylist}
						childPlaylists={children(playlist.key)}
						activePlaylistKey={activePlaylistKey}
					/>
				))}
			</div>
		</div>
	);
}

function PlaylistItem({
	playlist,
	isActive,
	onSelect,
	childPlaylists,
	activePlaylistKey,
}: {
	playlist: DjPlaylist;
	isActive: boolean;
	onSelect: (key: string) => void;
	childPlaylists: DjPlaylist[];
	activePlaylistKey: string | null;
}) {
	return (
		<div>
			<button
				type="button"
				onClick={() => onSelect(playlist.key)}
				className={cn(
					"w-full flex items-center gap-2 px-2 py-1.5 text-xs text-left transition-colors duration-150 hover:duration-0",
					isActive
						? "bg-muted text-foreground"
						: "text-muted-foreground hover:bg-muted/50 hover:text-foreground",
				)}
			>
				<Folder className="size-3.5 shrink-0" />
				<span className="truncate flex-1">{playlist.title}</span>
				{playlist.trackCount > 0 && (
					<span className="text-[10px] opacity-50">{playlist.trackCount}</span>
				)}
			</button>
			{childPlaylists.length > 0 && (
				<div className="ml-3">
					{childPlaylists.map((child) => (
						<button
							key={child.key}
							type="button"
							onClick={() => onSelect(child.key)}
							className={cn(
								"w-full flex items-center gap-2 px-2 py-1.5 text-xs text-left transition-colors duration-150 hover:duration-0",
								activePlaylistKey === child.key
									? "bg-muted text-foreground"
									: "text-muted-foreground hover:bg-muted/50 hover:text-foreground",
							)}
						>
							<Folder className="size-3 shrink-0" />
							<span className="truncate flex-1">{child.title}</span>
							{child.trackCount > 0 && (
								<span className="text-[10px] opacity-50">
									{child.trackCount}
								</span>
							)}
						</button>
					))}
				</div>
			)}
		</div>
	);
}

function TrackList() {
	const tracks = useDjImportStore((s) => s.tracks);
	const selectedKeys = useDjImportStore((s) => s.selectedKeys);
	const toggleTrackSelection = useDjImportStore((s) => s.toggleTrackSelection);
	const selectAllTracks = useDjImportStore((s) => s.selectAllTracks);
	const clearSelection = useDjImportStore((s) => s.clearSelection);
	const loading = useDjImportStore((s) => s.loading);

	if (loading) {
		return (
			<div className="flex-1 flex items-center justify-center text-xs text-muted-foreground">
				Loading tracks...
			</div>
		);
	}

	if (tracks.length === 0) {
		return (
			<div className="flex-1 flex items-center justify-center text-xs text-muted-foreground">
				No tracks found
			</div>
		);
	}

	const allSelected =
		tracks.length > 0 && tracks.every((t) => selectedKeys.has(t.key));

	return (
		<div className="flex-1 flex flex-col overflow-hidden">
			<div className="grid grid-cols-[32px_1fr_1fr_80px_60px] gap-3 px-3 py-2 text-[10px] font-medium text-muted-foreground border-b border-border/50 select-none">
				<div className="flex items-center justify-center">
					<Checkbox
						checked={allSelected}
						onCheckedChange={() =>
							allSelected ? clearSelection() : selectAllTracks()
						}
					/>
				</div>
				<div>TITLE</div>
				<div>ARTIST</div>
				<div className="text-right">BPM</div>
				<div className="text-right">TIME</div>
			</div>
			<div className="flex-1 overflow-y-auto">
				{tracks.map((track) => {
					const isSelected = selectedKeys.has(track.key);
					return (
						<button
							key={track.key}
							type="button"
							onClick={() => toggleTrackSelection(track.key)}
							className={cn(
								"w-full grid grid-cols-[32px_1fr_1fr_80px_60px] gap-3 px-3 py-1.5 text-xs items-center text-left transition-colors duration-150 hover:duration-0",
								isSelected ? "bg-primary/10" : "hover:bg-muted",
							)}
						>
							<div className="flex items-center justify-center">
								<Checkbox checked={isSelected} tabIndex={-1} />
							</div>
							<div className="font-medium truncate text-foreground/90">
								{track.title || track.filename || "Untitled"}
							</div>
							<div className="text-muted-foreground truncate">
								{track.artist || "Unknown"}
							</div>
							<div className="text-muted-foreground text-right font-mono">
								{track.bpm ? track.bpm.toFixed(1) : "--"}
							</div>
							<div className="text-muted-foreground text-right font-mono">
								{formatDuration(track.duration)}
							</div>
						</button>
					);
				})}
			</div>
		</div>
	);
}

function ImportProgress() {
	const importing = useDjImportStore((s) => s.importing);
	const { done, total } = useDjImportStore((s) => s.importProgress);
	const currentTrack = useDjImportStore((s) => s.currentImportTrack);

	if (!importing) return null;

	const pct = total > 0 ? Math.round((done / total) * 100) : 0;

	return (
		<div className="flex items-center gap-3 px-4 py-3 bg-muted/50 border-t border-border/50">
			<Loader2 className="size-4 animate-spin text-muted-foreground" />
			<div className="flex-1 min-w-0">
				<div className="text-xs font-medium">
					Importing tracks... {done}/{total}
				</div>
				{currentTrack && (
					<div className="text-xs text-muted-foreground truncate">
						{currentTrack}
					</div>
				)}
				<div className="mt-1 h-1.5 rounded-full bg-muted overflow-hidden">
					<div
						className="h-full bg-primary rounded-full transition-all duration-150"
						style={{ width: `${pct}%` }}
					/>
				</div>
			</div>
		</div>
	);
}
