import { X } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { useTrackEditorStore } from "../stores/use-track-editor-store";
import {
	useBarClassifications,
	useClassifierThresholds,
} from "./hooks/use-bar-classifications";

const DEFAULT_THRESHOLD = 0.5;
const STORAGE_VISIBLE = "luma:bar-tags-debug-visible";
const STORAGE_POS = "luma:bar-tags-debug-pos";

type Pos = { x: number; y: number };

function readPos(): Pos {
	try {
		const raw = localStorage.getItem(STORAGE_POS);
		if (raw) {
			const parsed = JSON.parse(raw) as Pos;
			if (Number.isFinite(parsed.x) && Number.isFinite(parsed.y)) return parsed;
		}
	} catch {
		// ignore
	}
	return { x: 16, y: 16 };
}

function readVisible(): boolean {
	try {
		const raw = localStorage.getItem(STORAGE_VISIBLE);
		if (raw === "0") return false;
	} catch {
		// ignore
	}
	return true;
}

export function BarTagsDebug() {
	const trackId = useTrackEditorStore((s) => s.trackId);
	const playheadPosition = useTrackEditorStore((s) => s.playheadPosition);
	const beatGrid = useTrackEditorStore((s) => s.beatGrid);
	const tags = useBarClassifications(trackId);
	const thresholds = useClassifierThresholds();

	const [visible, setVisible] = useState(readVisible);
	const [pos, setPos] = useState<Pos>(readPos);
	const dragRef = useRef<{
		startX: number;
		startY: number;
		baseX: number;
		baseY: number;
	} | null>(null);

	useEffect(() => {
		try {
			localStorage.setItem(STORAGE_POS, JSON.stringify(pos));
		} catch {
			// ignore
		}
	}, [pos]);

	useEffect(() => {
		try {
			localStorage.setItem(STORAGE_VISIBLE, visible ? "1" : "0");
		} catch {
			// ignore
		}
	}, [visible]);

	const currentBar = useMemo(() => {
		if (!tags || !beatGrid?.downbeats?.length) return null;
		// Same bar index the Timecode display uses: largest i where
		// downbeats[i] <= playhead. Then just look it up in classifications.
		let idx = -1;
		for (let i = 0; i < beatGrid.downbeats.length; i++) {
			if (beatGrid.downbeats[i] <= playheadPosition) idx = i;
			else break;
		}
		if (idx < 0) return null;
		return tags.classifications.find((c) => c.bar_idx === idx) ?? null;
	}, [tags, playheadPosition, beatGrid]);

	const handleMouseDown = (e: React.MouseEvent<HTMLDivElement>) => {
		dragRef.current = {
			startX: e.clientX,
			startY: e.clientY,
			baseX: pos.x,
			baseY: pos.y,
		};
		const handleMove = (ev: MouseEvent) => {
			const d = dragRef.current;
			if (!d) return;
			setPos({
				x: Math.max(0, d.baseX + (ev.clientX - d.startX)),
				y: Math.max(0, d.baseY + (ev.clientY - d.startY)),
			});
		};
		const handleUp = () => {
			dragRef.current = null;
			window.removeEventListener("mousemove", handleMove);
			window.removeEventListener("mouseup", handleUp);
		};
		window.addEventListener("mousemove", handleMove);
		window.addEventListener("mouseup", handleUp);
	};

	if (!visible || !trackId) return null;

	const items = currentBar
		? Object.entries(currentBar.predictions ?? {})
				.filter(
					([k, v]) =>
						k !== "intensity" && v >= (thresholds[k] ?? DEFAULT_THRESHOLD),
				)
				.sort((a, b) => b[1] - a[1])
		: [];
	const intensity =
		currentBar && typeof currentBar.predictions.intensity === "number"
			? currentBar.predictions.intensity
			: null;

	return (
		<div
			className="fixed z-40 select-none rounded-sm border border-border/60 bg-background/85 backdrop-blur-sm shadow-lg font-mono text-[11px]"
			style={{ left: pos.x, top: pos.y, width: 180 }}
		>
			{/* Title bar */}
			{/* biome-ignore lint/a11y/noStaticElementInteractions: drag handle */}
			<div
				className="flex items-center justify-between px-1.5 py-0.5 cursor-move bg-muted/60 border-b border-border/50"
				onMouseDown={handleMouseDown}
			>
				<span className="text-[10px] uppercase tracking-wider text-muted-foreground">
					bar tags
				</span>
				<button
					type="button"
					onClick={() => setVisible(false)}
					className="text-muted-foreground hover:text-foreground"
				>
					<X className="size-3" />
				</button>
			</div>

			{/* Body */}
			<div className="px-1.5 py-1 space-y-0.5">
				{!tags ? (
					<div className="text-muted-foreground/70">no bar tags</div>
				) : !currentBar ? (
					<div className="text-muted-foreground/70">no bar at playhead</div>
				) : (
					<>
						<div className="flex items-center justify-between text-muted-foreground/80">
							<span>bar {currentBar.bar_idx + 1}</span>
							{intensity !== null && <span>i={intensity.toFixed(2)}</span>}
						</div>
						{items.length === 0 ? (
							<div className="text-muted-foreground/60">—</div>
						) : (
							items.map(([name, prob]) => (
								<TagRow
									key={name}
									name={name}
									prob={prob}
									threshold={thresholds[name] ?? DEFAULT_THRESHOLD}
								/>
							))
						)}
					</>
				)}
			</div>
		</div>
	);
}

function TagRow({
	name,
	prob,
	threshold,
}: {
	name: string;
	prob: number;
	threshold: number;
}) {
	const probPct = Math.min(100, prob * 100);
	const threshPct = Math.min(100, Math.max(0, threshold * 100));
	return (
		<div className="flex items-center gap-1.5">
			<div className="flex-1 truncate text-foreground/90">{name}</div>
			<div className="relative w-12 h-1 bg-muted/60 rounded-sm overflow-hidden">
				<div
					className="h-full bg-emerald-500/70"
					style={{ width: `${probPct}%` }}
				/>
				<div
					className="absolute top-0 h-full w-px bg-foreground/70"
					style={{ left: `${threshPct}%` }}
					title={`threshold ${threshold.toFixed(3)}`}
				/>
			</div>
			<div className="w-7 text-right text-muted-foreground tabular-nums">
				{prob.toFixed(2)}
			</div>
		</div>
	);
}
