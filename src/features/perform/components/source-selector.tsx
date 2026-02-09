import { cn } from "@/shared/lib/utils";

interface SourceSelectorProps {
	onSelect: (source: "stagelinq") => void;
}

export function SourceSelector({ onSelect }: SourceSelectorProps) {
	return (
		<div className="flex items-center justify-center h-full">
			<div className="flex gap-4">
				{/* Pioneer - coming soon */}
				<div
					className={cn(
						"w-64 border border-border/40 bg-background/50 p-6 opacity-40 cursor-not-allowed select-none",
					)}
				>
					<div className="text-xs text-muted-foreground mb-1">Coming soon</div>
					<div className="text-sm font-medium text-foreground/60">
						Pioneer Pro DJ Link
					</div>
					<div className="mt-2 text-xs text-muted-foreground">
						Connect to Pioneer CDJs via Pro DJ Link protocol
					</div>
				</div>

				{/* Denon StageLinQ */}
				<button
					type="button"
					onClick={() => onSelect("stagelinq")}
					className={cn(
						"w-64 border border-border bg-background p-6 text-left transition-colors",
						"hover:border-foreground/30 hover:bg-foreground/5",
					)}
				>
					<div className="text-xs text-muted-foreground mb-1">Available</div>
					<div className="text-sm font-medium text-foreground">
						Denon DJ StageLinQ
					</div>
					<div className="mt-2 text-xs text-muted-foreground">
						Connect to Denon SC6000, SC Live, Prime 4, and other StageLinQ
						devices
					</div>
				</button>
			</div>
		</div>
	);
}
