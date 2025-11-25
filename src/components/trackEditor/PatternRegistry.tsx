import { useTrackEditorStore } from "@/useTrackEditorStore";
import type { PatternSummary } from "@/bindings/schema";

const patternColors = [
	"#8b5cf6", "#ec4899", "#f59e0b", "#10b981",
	"#3b82f6", "#ef4444", "#06b6d4", "#f97316",
];

function getPatternColor(patternId: number): string {
	return patternColors[patternId % patternColors.length];
}

export function PatternRegistry() {
	const patterns = useTrackEditorStore((s) => s.patterns);
	const patternsLoading = useTrackEditorStore((s) => s.patternsLoading);
	const setDraggingPatternId = useTrackEditorStore((s) => s.setDraggingPatternId);

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
	onDragStart: () => void;
	onDragEnd: () => void;
};

function PatternItem({ pattern, color, onDragStart }: PatternItemProps) {
	const handleMouseDown = (e: React.MouseEvent) => {
		if (e.button !== 0) return; // Only left click
		console.log("[PatternItem] Mouse down (start drag)", { id: pattern.id, name: pattern.name });
		onDragStart();
	};

	return (
		<div
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

			{/* Drag handle indicator */}
			<div className="opacity-0 group-hover:opacity-30 text-[10px] text-muted-foreground">
				⋮⋮
			</div>
		</div>
	);
}
