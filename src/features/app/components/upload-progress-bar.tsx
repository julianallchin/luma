import { Upload } from "lucide-react";
import { useUploadProgressStore } from "../stores/use-upload-progress-store";

export function UploadProgressBar() {
	const total = useUploadProgressStore((s) => s.total);
	const completed = useUploadProgressStore((s) => s.completed);
	const active = total > 0 && completed < total;

	if (!active) return null;

	const pct = total > 0 ? Math.round((completed / total) * 100) : 0;

	return (
		<div className="flex items-center gap-3 px-4 h-6 shrink-0 border-t border-border/20">
			<Upload className="size-3 text-muted-foreground/60 shrink-0" />
			<div className="flex items-center gap-2 flex-1 min-w-0">
				<span className="text-[10px] text-muted-foreground/60 tracking-wider shrink-0">
					UPLOADING{" "}
					<span className="text-muted-foreground">
						{completed}/{total}
					</span>
				</span>
				<div className="flex-1 h-1 rounded-full bg-muted overflow-hidden">
					<div
						className="h-full bg-primary rounded-full transition-all duration-150"
						style={{ width: `${pct}%` }}
					/>
				</div>
			</div>
		</div>
	);
}
