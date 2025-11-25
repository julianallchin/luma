import {
	Popover,
	PopoverContent,
	PopoverTrigger,
} from "@/shared/components/ui/popover";
import type { RenderMetrics } from "../types/timeline-types";

type TimelineMetricsProps = {
	metrics: RenderMetrics;
};

export function TimelineMetrics({ metrics }: TimelineMetricsProps) {
	const rankedSections = [
		{ key: "ruler", label: "ruler/grid", value: metrics.avg.ruler },
		{ key: "waveform", label: "waveform", value: metrics.avg.waveform },
		{
			key: "annotations",
			label: "annotations",
			value: metrics.avg.annotations,
		},
		{ key: "minimap", label: "minimap", value: metrics.avg.minimap },
	]
		.sort((a, b) => b.value - a.value)
		.slice(0, 3);

	return (
		<Popover>
			<PopoverTrigger asChild>
				<button
					type="button"
					className="absolute bottom-2 right-2 px-2 py-1 bg-neutral-900/90 rounded text-[10px] text-neutral-200 font-mono backdrop-blur-sm border border-neutral-800 shadow-sm hover:border-neutral-700 transition-colors"
				>
					{(metrics.drawFps || 0).toFixed(0)} fps
				</button>
			</PopoverTrigger>
			<PopoverContent className="w-72 text-[11px] font-mono bg-neutral-950 border-neutral-800 text-neutral-200">
				<div className="space-y-1">
					<div className="flex justify-between">
						<span>draw fps</span>
						<span>{(metrics.drawFps || 0).toFixed(1)}</span>
					</div>
					<div className="flex justify-between text-neutral-400">
						<span>rAF fps</span>
						<span>{(metrics.rafFps || 0).toFixed(1)}</span>
					</div>
					<div className="flex justify-between">
						<span>frame total</span>
						<span>{metrics.totalMs.toFixed(2)} ms</span>
					</div>
					<div className="flex justify-between text-neutral-400">
						<span>avg total</span>
						<span>{metrics.avg.totalMs.toFixed(2)} ms</span>
					</div>
					<div className="flex justify-between text-neutral-400">
						<span>peak total</span>
						<span>{metrics.peak.totalMs.toFixed(2)} ms</span>
					</div>
					<div className="h-px bg-neutral-800 my-2" />
					{rankedSections.map((s) => (
						<div
							key={s.key}
							className="flex justify-between font-semibold text-neutral-100"
						>
							<span>{s.label} (avg)</span>
							<span>{s.value.toFixed(2)} ms</span>
						</div>
					))}
					<div className="h-px bg-neutral-800 my-2" />
					<div className="grid grid-cols-2 gap-x-2 text-neutral-300">
						<span>ruler</span>
						<span className="text-right">
							{metrics.sections.ruler.toFixed(2)} /{" "}
							{metrics.avg.ruler.toFixed(2)} / {metrics.peak.ruler.toFixed(2)}{" "}
							ms
						</span>
						<span>waveform</span>
						<span className="text-right">
							{metrics.sections.waveform.toFixed(2)} /{" "}
							{metrics.avg.waveform.toFixed(2)} /{" "}
							{metrics.peak.waveform.toFixed(2)} ms
						</span>
						<span>annotations</span>
						<span className="text-right">
							{metrics.sections.annotations.toFixed(2)} /{" "}
							{metrics.avg.annotations.toFixed(2)} /{" "}
							{metrics.peak.annotations.toFixed(2)} ms
						</span>
						<span>minimap</span>
						<span className="text-right">
							{metrics.sections.minimap.toFixed(2)} /{" "}
							{metrics.avg.minimap.toFixed(2)} /{" "}
							{metrics.peak.minimap.toFixed(2)} ms
						</span>
					</div>
					<div className="text-[10px] text-neutral-500 pt-2">
						Now/avg/peak per section. Samples every 5 frames.
					</div>
				</div>
			</PopoverContent>
		</Popover>
	);
}
