import type { TrackBrowserRow } from "@/bindings/schema";
import { cn } from "@/shared/lib/utils";

type Step = { label: string; active: boolean };

function buildSteps(track: TrackBrowserRow): Step[] {
	return [
		{ label: "Uploaded", active: track.hasStorage },
		{ label: "Beats", active: track.hasBeats },
		{ label: "Stems", active: track.hasStems },
		{ label: "Chords", active: track.hasRoots },
		{ label: "Drums", active: track.hasDrumOnsets },
		{ label: "Bars", active: track.hasBarClassifications },
	];
}

const SIZE = 14;
const STROKE = 2;
const RADIUS = (SIZE - STROKE) / 2;
const CIRCUMFERENCE = 2 * Math.PI * RADIUS;

/// Tiny progress ring rendered per track row. Uses a native `title`
/// attribute instead of a Radix HoverCard — with hundreds of rows the
/// portal/context overhead per row was a real cost during scroll.
export function PreprocessingStatus({ track }: { track: TrackBrowserRow }) {
	const steps = buildSteps(track);
	const completed = steps.filter((s) => s.active).length;
	const fraction = completed / steps.length;
	const dashOffset = CIRCUMFERENCE * (1 - fraction);
	const done = completed === steps.length;
	const tip = steps.map((s) => `${s.active ? "✓" : "·"} ${s.label}`).join("\n");

	return (
		<div
			className="flex items-center justify-center cursor-default"
			title={tip}
		>
			<svg
				width={SIZE}
				height={SIZE}
				viewBox={`0 0 ${SIZE} ${SIZE}`}
				className="-rotate-90"
				role="img"
				aria-label={`Preprocessing: ${completed} of ${steps.length} steps complete`}
			>
				<circle
					cx={SIZE / 2}
					cy={SIZE / 2}
					r={RADIUS}
					fill="none"
					strokeWidth={STROKE}
					className="stroke-muted-foreground/20"
				/>
				<circle
					cx={SIZE / 2}
					cy={SIZE / 2}
					r={RADIUS}
					fill="none"
					strokeWidth={STROKE}
					strokeDasharray={CIRCUMFERENCE}
					strokeDashoffset={dashOffset}
					strokeLinecap="round"
					className={cn(
						"transition-[stroke-dashoffset] duration-300",
						done ? "stroke-emerald-500" : "stroke-yellow-500",
					)}
				/>
			</svg>
		</div>
	);
}
