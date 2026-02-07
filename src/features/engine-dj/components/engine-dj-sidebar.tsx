import { Folder, Library } from "lucide-react";
import { cn } from "@/shared/lib/utils";
import { useEngineDjStore } from "../stores/use-engine-dj-store";

export function EngineDjSidebar() {
	const playlists = useEngineDjStore((s) => s.playlists);
	const activeView = useEngineDjStore((s) => s.activeView);
	const activePlaylistId = useEngineDjStore((s) => s.activePlaylistId);
	const selectPlaylist = useEngineDjStore((s) => s.selectPlaylist);
	const libraryInfo = useEngineDjStore((s) => s.libraryInfo);

	// Build tree: top-level playlists have no parent or parentId = 0
	const topLevel = playlists.filter((p) => !p.parentId || p.parentId === 0);
	const children = (parentId: number) =>
		playlists.filter((p) => p.parentId === parentId);

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
						activeView === "all" && activePlaylistId === null
							? "bg-muted text-foreground"
							: "text-muted-foreground hover:bg-muted/50 hover:text-foreground",
					)}
				>
					<Library className="size-3.5 shrink-0" />
					All Tracks
				</button>

				{topLevel.map((playlist) => (
					<PlaylistItem
						key={playlist.id}
						id={playlist.id}
						title={playlist.title}
						trackCount={playlist.trackCount}
						isActive={activePlaylistId === playlist.id}
						onSelect={selectPlaylist}
						childPlaylists={children(playlist.id)}
						activePlaylistId={activePlaylistId}
					/>
				))}
			</div>
		</div>
	);
}

function PlaylistItem({
	id,
	title,
	trackCount,
	isActive,
	onSelect,
	childPlaylists,
	activePlaylistId,
}: {
	id: number;
	title: string;
	trackCount: number;
	isActive: boolean;
	onSelect: (id: number) => void;
	childPlaylists: { id: number; title: string; trackCount: number }[];
	activePlaylistId: number | null;
}) {
	return (
		<div>
			<button
				type="button"
				onClick={() => onSelect(id)}
				className={cn(
					"w-full flex items-center gap-2 px-2 py-1.5 text-xs text-left transition-colors duration-150 hover:duration-0",
					isActive
						? "bg-muted text-foreground"
						: "text-muted-foreground hover:bg-muted/50 hover:text-foreground",
				)}
			>
				<Folder className="size-3.5 shrink-0" />
				<span className="truncate flex-1">{title}</span>
				<span className="text-[10px] opacity-50">{trackCount}</span>
			</button>
			{childPlaylists.length > 0 && (
				<div className="ml-3">
					{childPlaylists.map((child) => (
						<button
							key={child.id}
							type="button"
							onClick={() => onSelect(child.id)}
							className={cn(
								"w-full flex items-center gap-2 px-2 py-1.5 text-xs text-left transition-colors duration-150 hover:duration-0",
								activePlaylistId === child.id
									? "bg-muted text-foreground"
									: "text-muted-foreground hover:bg-muted/50 hover:text-foreground",
							)}
						>
							<Folder className="size-3 shrink-0" />
							<span className="truncate flex-1">{child.title}</span>
							<span className="text-[10px] opacity-50">{child.trackCount}</span>
						</button>
					))}
				</div>
			)}
		</div>
	);
}
