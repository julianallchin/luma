import { Pencil } from "lucide-react";
import { useLocation, useNavigate } from "react-router-dom";
import type { PatternSummary } from "@/bindings/schema";
import { useTrackEditorStore } from "../stores/use-track-editor-store";

const patternColors = [
	"#8b5cf6",
	"#ec4899",
	"#f59e0b",
	"#10b981",
	"#3b82f6",
	"#ef4444",
	"#06b6d4",
	"#f97316",
];

function getPatternColor(patternId: number): string {
	return patternColors[patternId % patternColors.length];
}

export function PatternRegistry() {
	const patterns = useTrackEditorStore((s) => s.patterns);
	const patternsLoading = useTrackEditorStore((s) => s.patternsLoading);
	const setDraggingPatternId = useTrackEditorStore(
		(s) => s.setDraggingPatternId,
	);
	const trackName = useTrackEditorStore((s) => s.trackName);
	const backLabel = trackName || "Track";

	if (patternsLoading) {
		return (
			<div className="p-4 text-xs text-muted-foreground">
				Loading patterns...
			</div>
		);
	}

	if (patterns.length === 0) {
		return (
			<div className="p-4 text-xs text-muted-foreground text-center">
				<div className="opacity-50 mb-2">No patterns yet</div>
				<div className="text-[10px]">Create patterns in the Library</div>
			</div>
		);
	}

	return (
		<div className="p-2 space-y-1">
			{patterns.map((pattern) => (
				<PatternItem
					key={pattern.id}
					pattern={pattern}
					color={getPatternColor(pattern.id)}
					backLabel={backLabel}
					onDragStart={() => setDraggingPatternId(pattern.id)}
					onDragEnd={() => {}}
				/>
			))}
		</div>
	);
}

type PatternItemProps = {
	pattern: PatternSummary;
	color: string;
	backLabel: string;
	onDragStart: () => void;
	onDragEnd: () => void;
};

function PatternItem({
	pattern,
	color,
	backLabel,
	onDragStart,
}: PatternItemProps) {
	const navigate = useNavigate();
	const location = useLocation();

	const handleMouseDown = (e: React.MouseEvent) => {
		if (e.button !== 0) return; // Only left click
		console.log("[PatternItem] Mouse down (start drag)", {
			id: pattern.id,
			name: pattern.name,
		});
		onDragStart();
	};

	const handleEditClick = (e: React.MouseEvent) => {
		e.stopPropagation();
		navigate(`/pattern/${pattern.id}`, {
			state: {
				name: pattern.name,
				from: `${location.pathname}${location.search}`,
				backLabel,
			},
		});
	};

	return (
		<button
			type="button"
			aria-label="Drag to add pattern"
			onMouseDown={handleMouseDown}
			className="group flex items-center gap-2 px-2 py-1.5 rounded cursor-grab active:cursor-grabbing hover:bg-muted/50 transition-colors select-none"
		>
			{/* Color indicator */}
			<div
				className="w-3 h-3 rounded-sm flex-shrink-0"
				style={{ backgroundColor: color }}
			/>

			{/* Pattern info */}
			<div className="flex-1 min-w-0">
				<div className="text-xs font-medium truncate text-foreground/90">
					{pattern.name}
				</div>
				{pattern.description && (
					<div className="text-[10px] text-muted-foreground truncate">
						{pattern.description}
					</div>
				)}
			</div>

			{/* Edit button */}
			<button
				type="button"
				onMouseDown={(e) => e.stopPropagation()}
				onClick={handleEditClick}
				className="opacity-0 group-hover:opacity-70 text-muted-foreground hover:text-foreground transition-colors p-1 rounded hover:bg-muted"
				aria-label={`Edit ${pattern.name}`}
			>
				<Pencil className="w-3.5 h-3.5" />
			</button>
		</button>
	);
}
