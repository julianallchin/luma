import { ChevronLeft } from "lucide-react";
import { CreatePatternDialog } from "@/features/patterns/components/create-pattern-dialog";
import { Button } from "@/shared/components/ui/button";
import { useTrackEditorStore } from "../stores/use-track-editor-store";
import { PatternRegistry } from "./pattern-registry";

export function TrackSidebar() {
	const resetTrack = useTrackEditorStore((s) => s.resetTrack);
	const loadPatterns = useTrackEditorStore((s) => s.loadPatterns);

	return (
		<div className="w-80 border-r border-border flex flex-col bg-background/50">
			<div className="p-3 border-b border-border/50 flex items-center justify-between gap-2">
				<div className="flex items-center gap-2">
					<button
						type="button"
						onClick={() => {
							resetTrack();
						}}
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
						<Button variant="outline" size="sm" className="h-7 px-2 text-xs">
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
	);
}
