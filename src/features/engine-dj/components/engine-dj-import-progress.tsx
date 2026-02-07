import { Loader2 } from "lucide-react";
import { useEngineDjStore } from "../stores/use-engine-dj-store";

export function EngineDjImportProgress() {
	const importing = useEngineDjStore((s) => s.importing);
	const { done, total } = useEngineDjStore((s) => s.importProgress);

	if (!importing) return null;

	const pct = total > 0 ? Math.round((done / total) * 100) : 0;

	return (
		<div className="flex items-center gap-3 px-4 py-3 bg-muted/50 border-t border-border/50">
			<Loader2 className="size-4 animate-spin text-muted-foreground" />
			<div className="flex-1">
				<div className="text-xs font-medium">
					Importing tracks... {done}/{total}
				</div>
				<div className="mt-1 h-1.5 rounded-full bg-muted overflow-hidden">
					<div
						className="h-full bg-primary rounded-full transition-all"
						style={{ width: `${pct}%` }}
					/>
				</div>
			</div>
		</div>
	);
}
