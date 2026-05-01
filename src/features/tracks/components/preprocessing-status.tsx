import { Check } from "lucide-react";
import type { TrackBrowserRow } from "@/bindings/schema";
import {
	HoverCard,
	HoverCardContent,
	HoverCardTrigger,
} from "@/shared/components/ui/hover-card";
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

export function PreprocessingStatus({ track }: { track: TrackBrowserRow }) {
	const steps = buildSteps(track);
	const completed = steps.filter((s) => s.active).length;
	const fraction = completed / steps.length;
	const dashOffset = CIRCUMFERENCE * (1 - fraction);

	return (
		<HoverCard openDelay={300} closeDelay={100}>
			<HoverCardTrigger asChild>
				<div className="flex items-center justify-center cursor-default">
					<svg
						width={SIZE}
						height={SIZE}
						viewBox={`0 0 ${SIZE} ${SIZE}`}
						className="-rotate-90"
						role="img"
					>
						<title>
							{`Preprocessing: ${completed} of ${steps.length} steps complete`}
						</title>
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
							className="stroke-emerald-500 transition-[stroke-dashoffset] duration-300"
						/>
					</svg>
				</div>
			</HoverCardTrigger>
			<HoverCardContent className="w-36 p-2" side="left">
				<div className="flex flex-col gap-1.5">
					{steps.map((step) => (
						<div key={step.label} className="flex items-center gap-2">
							<div
								className={cn(
									"size-3 rounded-sm border flex items-center justify-center shrink-0",
									step.active
										? "bg-emerald-500 border-emerald-500"
										: "border-muted-foreground/40",
								)}
							>
								{step.active && (
									<Check className="size-2 text-white" strokeWidth={3} />
								)}
							</div>
							<span className="text-xs text-muted-foreground">
								{step.label}
							</span>
						</div>
					))}
				</div>
			</HoverCardContent>
		</HoverCard>
	);
}
