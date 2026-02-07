import { FileCode, Pencil } from "lucide-react";
import { useState } from "react";
import { useLocation, useNavigate } from "react-router-dom";
import type { PatternSummary } from "@/bindings/schema";
import { Button } from "@/shared/components/ui/button";
import {
	Dialog,
	DialogContent,
	DialogHeader,
	DialogTitle,
} from "@/shared/components/ui/dialog";
import {
	HoverCard,
	HoverCardContent,
	HoverCardTrigger,
} from "@/shared/components/ui/hover-card";
import type { TimelineAnnotation } from "../stores/use-track-editor-store";
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

type BeatGrid = {
	bpm: number;
	beatsPerBar: number;
	beats: number[];
	downbeats: number[];
};

function secondsToBarBeat(seconds: number, beatGrid: BeatGrid | null): string {
	if (!beatGrid || !beatGrid.downbeats.length || !beatGrid.beats.length) {
		return seconds.toFixed(2);
	}

	// Find which bar we're in by finding the last downbeat <= seconds
	let barIndex = 0;
	for (let i = 0; i < beatGrid.downbeats.length; i++) {
		if (beatGrid.downbeats[i] <= seconds) {
			barIndex = i;
		} else {
			break;
		}
	}

	const barStart = beatGrid.downbeats[barIndex];
	const barNumber = barIndex + 1;

	// Find which beat within this bar
	// Count beats that are >= barStart and <= seconds
	let beatInBar = 1;
	for (const beat of beatGrid.beats) {
		if (beat > barStart && beat <= seconds) {
			beatInBar++;
		}
	}

	// Clamp beat to beatsPerBar
	beatInBar = Math.min(beatInBar, beatGrid.beatsPerBar);

	return `${barNumber}.${beatInBar}`;
}

// Named colors for DSL output
const NAMED_COLORS: Record<string, [number, number, number]> = {
	red: [255, 0, 0],
	orange: [255, 165, 0],
	yellow: [255, 255, 0],
	green: [0, 255, 0],
	cyan: [0, 255, 255],
	blue: [0, 0, 255],
	purple: [128, 0, 128],
	magenta: [255, 0, 255],
	white: [255, 255, 255],
	black: [0, 0, 0],
};

function parseColorToRGB(value: unknown): [number, number, number] | null {
	if (typeof value === "string") {
		// Handle hex colors
		const hex = value.replace("#", "");
		if (/^[0-9a-fA-F]{6}$/.test(hex)) {
			return [
				Number.parseInt(hex.slice(0, 2), 16),
				Number.parseInt(hex.slice(2, 4), 16),
				Number.parseInt(hex.slice(4, 6), 16),
			];
		}
		if (/^[0-9a-fA-F]{3}$/.test(hex)) {
			return [
				Number.parseInt(hex[0] + hex[0], 16),
				Number.parseInt(hex[1] + hex[1], 16),
				Number.parseInt(hex[2] + hex[2], 16),
			];
		}
	}
	// Handle {r, g, b} object
	if (
		typeof value === "object" &&
		value !== null &&
		"r" in value &&
		"g" in value &&
		"b" in value
	) {
		const obj = value as { r: number; g: number; b: number };
		return [obj.r, obj.g, obj.b];
	}
	// Handle [r, g, b] array
	if (Array.isArray(value) && value.length >= 3) {
		return [value[0], value[1], value[2]];
	}
	return null;
}

function findClosestColorName(rgb: [number, number, number]): string {
	let closestName = "white";
	let closestDistance = Number.POSITIVE_INFINITY;

	for (const [name, namedRgb] of Object.entries(NAMED_COLORS)) {
		// Euclidean distance in RGB space
		const distance = Math.sqrt(
			(rgb[0] - namedRgb[0]) ** 2 +
				(rgb[1] - namedRgb[1]) ** 2 +
				(rgb[2] - namedRgb[2]) ** 2,
		);
		if (distance < closestDistance) {
			closestDistance = distance;
			closestName = name;
		}
	}

	return closestName;
}

function formatArgValue(key: string, value: unknown): string {
	// Check if this looks like a color argument
	const isColorKey = /color/i.test(key);
	if (isColorKey) {
		const rgb = parseColorToRGB(value);
		if (rgb) {
			return findClosestColorName(rgb);
		}
	}

	if (typeof value === "string") return value;
	if (typeof value === "number") return String(value);
	if (typeof value === "boolean") return String(value);
	if (value === null || value === undefined) return "null";
	return JSON.stringify(value);
}

function convertAnnotationsToDSL(
	annotations: TimelineAnnotation[],
	patterns: PatternSummary[],
	beatGrid: BeatGrid | null,
	trackName: string,
): string {
	if (annotations.length === 0) {
		return "# No annotations";
	}

	const patternMap = new Map(patterns.map((p) => [p.id, p]));
	const sorted = [...annotations].sort((a, b) => a.startTime - b.startTime);

	// Group annotations that share the same time range
	type TimeRange = {
		startTime: number;
		endTime: number;
		layers: Map<number, TimelineAnnotation>;
	};

	const ranges: TimeRange[] = [];

	for (const ann of sorted) {
		// Find an existing range that matches this annotation's times
		let range = ranges.find(
			(r) => r.startTime === ann.startTime && r.endTime === ann.endTime,
		);
		if (!range) {
			range = {
				startTime: ann.startTime,
				endTime: ann.endTime,
				layers: new Map(),
			};
			ranges.push(range);
		}
		range.layers.set(ann.zIndex, ann);
	}

	// Sort ranges by start time
	ranges.sort((a, b) => a.startTime - b.startTime);

	// Find all unique z-indices across all annotations and normalize to start at 0
	const allZIndices = new Set<number>();
	for (const ann of annotations) {
		allZIndices.add(ann.zIndex);
	}
	const sortedZIndices = Array.from(allZIndices).sort((a, b) => a - b);
	const minZIndex = sortedZIndices.length > 0 ? sortedZIndices[0] : 0;

	const lines: string[] = [];
	lines.push(`# ${trackName}`);
	lines.push(`# Generated from ${annotations.length} annotations`);
	if (beatGrid) {
		lines.push(
			`# BPM: ${beatGrid.bpm}, Time Signature: ${beatGrid.beatsPerBar}/4`,
		);
	}
	lines.push("");

	for (const range of ranges) {
		const startBeat = secondsToBarBeat(range.startTime, beatGrid);
		const endBeat = secondsToBarBeat(range.endTime, beatGrid);
		lines.push(`${startBeat}-${endBeat}:`);

		for (const zIndex of sortedZIndices) {
			const ann = range.layers.get(zIndex);
			if (!ann) continue; // Skip missing layers

			const normalizedZ = zIndex - minZIndex;
			const pattern = patternMap.get(ann.patternId);
			const patternName = pattern?.name ?? `pattern_${ann.patternId}`;
			const safeName = patternName.toLowerCase().replace(/\s+/g, "_");

			// Format args
			const args = ann.args as Record<string, unknown> | undefined;
			let argsStr = "";
			if (args && Object.keys(args).length > 0) {
				const argParts = Object.entries(args).map(
					([key, val]) => `${key}=${formatArgValue(key, val)}`,
				);
				argsStr = `(${argParts.join(", ")})`;
			} else {
				argsStr = "()";
			}

			lines.push(`  z${normalizedZ}: ${safeName}${argsStr}`);
		}
		lines.push("");
	}

	return lines.join("\n");
}

export function PatternRegistry() {
	const patterns = useTrackEditorStore((s) => s.patterns);
	const patternsLoading = useTrackEditorStore((s) => s.patternsLoading);
	const setDraggingPatternId = useTrackEditorStore(
		(s) => s.setDraggingPatternId,
	);
	const trackName = useTrackEditorStore((s) => s.trackName);
	const annotations = useTrackEditorStore((s) => s.annotations);
	const beatGrid = useTrackEditorStore((s) => s.beatGrid);
	const backLabel = trackName || "Track";

	const [dslDialogOpen, setDslDialogOpen] = useState(false);
	const [dslOutput, setDslOutput] = useState("");

	const handleExportDSL = () => {
		const dsl = convertAnnotationsToDSL(
			annotations,
			patterns,
			beatGrid,
			trackName || "Untitled Track",
		);
		setDslOutput(dsl);
		setDslDialogOpen(true);
	};

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
		<>
			<div className="flex flex-col h-full">
				<div className="flex-1">
					{patterns.map((pattern) => (
						<PatternItem
							key={pattern.id}
							pattern={pattern}
							color={getPatternColor(pattern.id)}
							backLabel={backLabel}
							onDragStart={(origin) => setDraggingPatternId(pattern.id, origin)}
							onDragEnd={() => {}}
						/>
					))}
				</div>
				<div className="p-3 border-t border-border/50">
					<Button
						variant="outline"
						size="sm"
						onClick={handleExportDSL}
						className="w-full gap-2"
					>
						<FileCode className="w-3.5 h-3.5" />
						Export DSL
					</Button>
				</div>
			</div>

			<Dialog open={dslDialogOpen} onOpenChange={setDslDialogOpen}>
				<DialogContent className="max-w-2xl max-h-[80vh]">
					<DialogHeader>
						<DialogTitle>Lighting Annotation DSL</DialogTitle>
					</DialogHeader>
					<textarea
						readOnly
						value={dslOutput}
						className="w-full h-[60vh] text-xs font-mono bg-muted p-4 rounded-md resize-none focus:outline-none"
					/>
				</DialogContent>
			</Dialog>
		</>
	);
}

type PatternItemProps = {
	pattern: PatternSummary;
	color: string;
	backLabel: string;
	onDragStart: (origin: { x: number; y: number }) => void;
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
		onDragStart({ x: e.clientX, y: e.clientY });
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
		<HoverCard openDelay={300} closeDelay={100}>
			<HoverCardTrigger asChild>
				<button
					type="button"
					aria-label="Drag to add pattern"
					onMouseDown={handleMouseDown}
					className="group w-full flex items-center gap-2 px-3 py-2 cursor-grab active:cursor-grabbing hover:bg-muted/50 transition-colors duration-150 hover:duration-0 select-none"
				>
					{/* Color indicator */}
					<div
						className="w-3 h-3 rounded-sm flex-shrink-0"
						style={{ backgroundColor: color }}
					/>

					{/* Pattern name */}
					<div className="flex-1 min-w-0 text-xs font-medium truncate text-foreground/90 text-left">
						{pattern.name}
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
			</HoverCardTrigger>
			{pattern.description && (
				<HoverCardContent side="right" align="start" className="w-56 text-xs">
					<p className="text-muted-foreground">{pattern.description}</p>
				</HoverCardContent>
			)}
		</HoverCard>
	);
}
