import type { DeckState } from "@/bindings/perform";
import { cn } from "@/shared/lib/utils";

interface DeckDisplayProps {
	deck: DeckState;
}

export function DeckDisplay({ deck }: DeckDisplayProps) {
	const bpm = deck.beat_bpm > 0 ? deck.beat_bpm : deck.bpm;
	const beatProgress =
		deck.total_beats > 0 ? (deck.beat / deck.total_beats) * 100 : 0;
	const beatInBar = deck.beat > 0 ? (Math.floor(deck.beat) % 4) + 1 : 0;

	return (
		<div className="border border-border bg-background p-4 flex-1 min-w-0">
			{/* Header */}
			<div className="flex items-center justify-between mb-3">
				<div className="flex items-center gap-2">
					<span className="text-xs text-muted-foreground font-mono">
						DECK {deck.id}
					</span>
					{deck.master && (
						<span className="text-[10px] font-medium bg-foreground text-background px-1.5 py-0.5">
							MASTER
						</span>
					)}
				</div>
				<div
					className={cn(
						"h-2 w-2 rounded-full",
						deck.playing ? "bg-green-500" : "bg-muted-foreground/30",
					)}
					title={deck.playing ? "Playing" : "Paused"}
				/>
			</div>

			{/* Track info */}
			<div className="mb-3 min-w-0">
				<div className="text-sm font-medium truncate">
					{deck.title ||
						(deck.song_loaded ? "Unknown Track" : "No track loaded")}
				</div>
				{deck.artist && (
					<div className="text-xs text-muted-foreground truncate">
						{deck.artist}
					</div>
				)}
			</div>

			{/* BPM */}
			<div className="flex items-baseline gap-1 mb-3">
				<span className="text-2xl font-mono font-medium tabular-nums">
					{bpm > 0 ? bpm.toFixed(1) : "---"}
				</span>
				<span className="text-xs text-muted-foreground">BPM</span>
			</div>

			{/* Beat position */}
			<div className="mb-3">
				<div className="flex items-center gap-2 mb-1">
					<span className="text-[10px] text-muted-foreground uppercase tracking-wider">
						Beat
					</span>
					<div className="flex gap-0.5">
						{[1, 2, 3, 4].map((b) => (
							<div
								key={b}
								className={cn(
									"w-3 h-3",
									Math.ceil(beatInBar) === b
										? "bg-foreground"
										: "bg-muted-foreground/20",
								)}
							/>
						))}
					</div>
				</div>
				{/* Track progress bar */}
				<div className="h-1 bg-muted-foreground/10 w-full">
					<div
						className="h-full bg-foreground/60 transition-[width] duration-75"
						style={{ width: `${Math.min(beatProgress, 100)}%` }}
					/>
				</div>
			</div>

			{/* Fader */}
			<div className="flex items-center gap-2">
				<span className="text-[10px] text-muted-foreground uppercase tracking-wider w-8">
					Vol
				</span>
				<div className="h-1 bg-muted-foreground/10 flex-1">
					<div
						className="h-full bg-foreground/40"
						style={{ width: `${(Math.min(deck.fader, 1) * 100).toFixed(0)}%` }}
					/>
				</div>
			</div>
		</div>
	);
}
