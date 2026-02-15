import * as React from "react";
import type { NodeProps } from "reactflow";
import { useHostAudioStore } from "@/features/patterns/stores/use-host-audio-store";
import { getCanvasColor, getCanvasColorRgba } from "@/shared/lib/canvas-colors";
import { BaseNode } from "./base-node";
import type { UvViewNodeData } from "./types";

const CANVAS_SIZE = 240;
const COLORS = [
	"#22d3ee",
	"#f472b6",
	"#a3e635",
	"#f59e0b",
	"#c084fc",
	"#60a5fa",
	"#fb7185",
	"#34d399",
];

function clamp(value: number, min: number, max: number) {
	return Math.min(max, Math.max(min, value));
}

export const UvViewNode = React.memo(function UvViewNode(
	props: NodeProps<UvViewNodeData>,
) {
	const { data } = props;
	const signal = data.viewSamples;

	const isLoaded = useHostAudioStore((state) => state.isLoaded);
	const currentTime = useHostAudioStore((state) => state.currentTime);
	const durationSeconds = useHostAudioStore((state) => state.durationSeconds);

	const canvasRef = React.useRef<HTMLCanvasElement>(null);

	const sampleTimeIndex = React.useMemo(() => {
		if (!signal || signal.t <= 1) return 0;
		if (!isLoaded || durationSeconds <= 0) return 0;
		const progress = clamp(currentTime / durationSeconds, 0, 1);
		return Math.round(progress * (signal.t - 1));
	}, [signal, isLoaded, currentTime, durationSeconds]);

	const sampledPoints = React.useMemo(() => {
		if (!signal) return [];
		if (signal.c < 2) return [];

		const n = Math.max(1, signal.n);
		const points: Array<{
			index: number;
			u: number;
			v: number;
			color: string;
		}> = [];
		for (let i = 0; i < n; i++) {
			const base = i * (signal.t * signal.c) + sampleTimeIndex * signal.c;
			const u = signal.data[base] ?? 0;
			const v = signal.data[base + 1] ?? 0;
			points.push({
				index: i,
				u,
				v,
				color: COLORS[i % COLORS.length],
			});
		}
		return points;
	}, [signal, sampleTimeIndex]);

	React.useEffect(() => {
		const canvas = canvasRef.current;
		if (!canvas) return;
		const ctx = canvas.getContext("2d");
		if (!ctx) return;

		const dpr = Math.max(window.devicePixelRatio ?? 1, 1);
		const size = CANVAS_SIZE;
		const scaled = Math.round(size * dpr);
		if (canvas.width !== scaled || canvas.height !== scaled) {
			canvas.width = scaled;
			canvas.height = scaled;
		}
		canvas.style.width = `${size}px`;
		canvas.style.height = `${size}px`;

		ctx.setTransform(1, 0, 0, 1, 0, 0);
		ctx.clearRect(0, 0, canvas.width, canvas.height);
		ctx.setTransform(dpr, 0, 0, dpr, 0, 0);

		// Grid + axes
		const center = size / 2;
		const radius = size * 0.42;

		const background = getCanvasColor("--background");
		const axisColor = getCanvasColorRgba("--muted-foreground", 0.28);
		const boundaryColor = getCanvasColorRgba("--border", 0.8);
		const centerColor = getCanvasColor("--foreground");

		ctx.fillStyle = background;
		ctx.fillRect(0, 0, size, size);

		ctx.strokeStyle = axisColor;
		ctx.lineWidth = 1;
		ctx.beginPath();
		ctx.moveTo(center, size * 0.05);
		ctx.lineTo(center, size * 0.95);
		ctx.moveTo(size * 0.05, center);
		ctx.lineTo(size * 0.95, center);
		ctx.stroke();

		// Unit circle boundary
		ctx.strokeStyle = boundaryColor;
		ctx.lineWidth = 1.5;
		ctx.beginPath();
		ctx.arc(center, center, radius, 0, Math.PI * 2);
		ctx.stroke();

		// Draw spotlight circles
		for (const point of sampledPoints) {
			const u = clamp(point.u, -1, 1);
			const v = clamp(point.v, -1, 1);
			const x = center + u * radius;
			const y = center - v * radius;

			ctx.fillStyle = `${point.color}33`;
			ctx.beginPath();
			ctx.arc(x, y, 13, 0, Math.PI * 2);
			ctx.fill();

			ctx.strokeStyle = point.color;
			ctx.lineWidth = 2;
			ctx.beginPath();
			ctx.arc(x, y, 9, 0, Math.PI * 2);
			ctx.stroke();

			ctx.fillStyle = point.color;
			ctx.beginPath();
			ctx.arc(x, y, 3, 0, Math.PI * 2);
			ctx.fill();
		}

		// Center marker
		ctx.fillStyle = centerColor;
		ctx.beginPath();
		ctx.arc(center, center, 2, 0, Math.PI * 2);
		ctx.fill();
	}, [sampledPoints]);

	const body = (
		<div className="px-2 py-2">
			{!signal ? (
				<div className="text-[11px] text-muted-foreground px-2 py-3 text-center">
					waiting for UV signal...
				</div>
			) : signal.c < 2 ? (
				<div className="text-[11px] text-destructive/90 px-2 py-3 text-center">
					UV viewer expects 2 channels
				</div>
			) : (
				<canvas
					ref={canvasRef}
					width={CANVAS_SIZE}
					height={CANVAS_SIZE}
					className="block rounded border border-border/60 bg-background"
					style={{ width: `${CANVAS_SIZE}px`, height: `${CANVAS_SIZE}px` }}
					role="img"
					aria-label="UV plane spotlight positions"
				/>
			)}
		</div>
	);

	return <BaseNode {...props} data={{ ...data, body }} />;
});
