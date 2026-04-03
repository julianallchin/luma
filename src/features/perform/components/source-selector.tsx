import { cn } from "@/shared/lib/utils";

interface SourceSelectorProps {
	onSelect: (source: "stagelinq" | "prodjlink") => void;
}

export function SourceSelector({ onSelect }: SourceSelectorProps) {
	return (
		<div className="flex items-center justify-center h-full">
			<div className="flex gap-4">
				{/* Pioneer Pro DJ Link */}
				<button
					type="button"
					onClick={() => onSelect("prodjlink")}
					className={cn(
						"w-64 border border-border bg-background p-6 text-left transition-colors",
						"hover:border-foreground/30 hover:bg-foreground/5",
					)}
				>
					<div className="text-xs text-muted-foreground mb-1">Available</div>
					<div className="text-sm font-medium text-foreground">
						Pioneer Pro DJ Link
					</div>
					<div className="mt-2 text-xs text-muted-foreground">
						Connect to Pioneer CDJ-2000NXS2, CDJ-3000, XDJ-RX3, and other Pro DJ
						Link devices
					</div>
				</button>

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
