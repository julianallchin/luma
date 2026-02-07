import { Checkbox } from "@/shared/components/ui/checkbox";
import { cn } from "@/shared/lib/utils";
import { useEngineDjStore } from "../stores/use-engine-dj-store";

const formatDuration = (seconds: number | null | undefined) => {
	if (seconds == null || Number.isNaN(seconds)) return "--:--";
	const total = Math.max(0, seconds);
	const minutes = Math.floor(total / 60);
	const secs = Math.floor(total % 60)
		.toString()
		.padStart(2, "0");
	return `${minutes}:${secs}`;
};

export function EngineDjTrackList() {
	const tracks = useEngineDjStore((s) => s.tracks);
	const selectedTrackIds = useEngineDjStore((s) => s.selectedTrackIds);
	const toggleTrackSelection = useEngineDjStore(
		(s) => s.toggleTrackSelection,
	);
	const selectAllTracks = useEngineDjStore((s) => s.selectAllTracks);
	const clearSelection = useEngineDjStore((s) => s.clearSelection);
	const loading = useEngineDjStore((s) => s.loading);

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
		tracks.length > 0 && tracks.every((t) => selectedTrackIds.has(t.id));

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
					const isSelected = selectedTrackIds.has(track.id);
					return (
						<button
							key={track.id}
							type="button"
							onClick={() => toggleTrackSelection(track.id)}
							className={cn(
								"w-full grid grid-cols-[32px_1fr_1fr_80px_60px] gap-3 px-3 py-1.5 text-xs items-center text-left transition-colors",
								isSelected
									? "bg-primary/10"
									: "hover:bg-muted",
							)}
						>
							<div className="flex items-center justify-center">
								<Checkbox checked={isSelected} tabIndex={-1} />
							</div>
							<div className="font-medium truncate text-foreground/90">
								{track.title || track.filename}
							</div>
							<div className="text-muted-foreground truncate">
								{track.artist || "Unknown"}
							</div>
							<div className="text-muted-foreground text-right font-mono">
								{track.bpmAnalyzed
									? track.bpmAnalyzed.toFixed(1)
									: "--"}
							</div>
							<div className="text-muted-foreground text-right font-mono">
								{formatDuration(track.length)}
							</div>
						</button>
					);
				})}
			</div>
		</div>
	);
}
