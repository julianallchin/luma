import { Trash2 } from "lucide-react";
import * as React from "react";
import type { NodeProps } from "reactflow";

import { useGraphStore } from "@/features/patterns/stores/use-graph-store";
import { Button } from "@/shared/components/ui/button";
import {
	Popover,
	PopoverAnchor,
	PopoverContent,
} from "@/shared/components/ui/popover";
import {
	ColorPicker,
	ColorPickerAlpha,
	ColorPickerHue,
	ColorPickerSelection,
} from "@/shared/components/ui/shadcn-io/color-picker";
import { BaseNode } from "./base-node";
import type { BaseNodeData } from "./types";

type Stop = { t: number; r: number; g: number; b: number; a: number };

export function GradientNode(props: NodeProps<BaseNodeData>) {
	const { data, id } = props;
	const params = useGraphStore(
		(state) => state.nodeParams[id] ?? ({} as Record<string, unknown>),
	);
	const setParam = useGraphStore((state) => state.setParam);

	const stopsParam = (params.stops as string) ?? "[]";
	let stops: Stop[] = [];
	try {
		stops = JSON.parse(stopsParam);
		if (!Array.isArray(stops)) stops = [];
	} catch {
		stops = [];
	}

	// Helper to update stops
	const updateStops = React.useCallback(
		(newStops: Stop[]) => {
			// Sort by t
			newStops.sort((a, b) => a.t - b.t);
			setParam(id, "stops", JSON.stringify(newStops));
		},
		[id, setParam],
	);

	// Helper to calculate interpolated color at t
	const getColorAt = (
		t: number,
	): { r: number; g: number; b: number; a: number } => {
		if (stops.length === 0) return { r: 1, g: 1, b: 1, a: 1 };
		if (stops.length === 1)
			return { r: stops[0].r, g: stops[0].g, b: stops[0].b, a: stops[0].a };

		// Clamp t
		const ct = Math.max(0, Math.min(1, t));

		if (ct <= stops[0].t)
			return { r: stops[0].r, g: stops[0].g, b: stops[0].b, a: stops[0].a };
		if (ct >= stops[stops.length - 1].t) {
			const last = stops[stops.length - 1];
			return { r: last.r, g: last.g, b: last.b, a: last.a };
		}

		for (let i = 0; i < stops.length - 1; i++) {
			const s1 = stops[i];
			const s2 = stops[i + 1];
			if (ct >= s1.t && ct <= s2.t) {
				const range = s2.t - s1.t;
				const mix = range > 0 ? (ct - s1.t) / range : 0;
				return {
					r: s1.r + (s2.r - s1.r) * mix,
					g: s1.g + (s2.g - s1.g) * mix,
					b: s1.b + (s2.b - s1.b) * mix,
					a: s1.a + (s2.a - s1.a) * mix,
				};
			}
		}
		return { r: 1, g: 1, b: 1, a: 1 };
	};

	const gradientCss = React.useMemo(() => {
		if (stops.length === 0) return "black";
		const stopsStr = stops
			.map(
				(s) =>
					`rgba(${s.r * 255},${s.g * 255},${s.b * 255},${s.a}) ${s.t * 100}%`,
			)
			.join(", ");
		return `linear-gradient(to right, ${stopsStr})`;
	}, [stops]);

	const [selectedStopIndex, setSelectedStopIndex] = React.useState<
		number | null
	>(null);
	const [draggingStopIndex, setDraggingStopIndex] = React.useState<
		number | null
	>(null);
	// We track drag start position to differentiate click vs drag
	const [dragStartPos, setDragStartPos] = React.useState<{
		x: number;
		y: number;
	} | null>(null);
	const containerRef = React.useRef<HTMLDivElement>(null);

	const handleTrackClick = (e: React.MouseEvent<HTMLDivElement>) => {
		// Only add if not dragging and target is the track itself (or close to it)
		if (draggingStopIndex !== null) return;

		const rect = e.currentTarget.getBoundingClientRect();
		const t = Math.max(0, Math.min(1, (e.clientX - rect.left) / rect.width));

		const color = getColorAt(t);
		const newStop = { t, ...color };
		const newStops = [...stops, newStop];
		// Sorting happens in updateStops, but we want to select the new stop.
		// Since sorting changes indices, we need to find it again.
		// We'll temporarily sort local copy to find index.
		newStops.sort((a, b) => a.t - b.t);
		const newIndex = newStops.indexOf(newStop);

		updateStops(newStops);
		setSelectedStopIndex(newIndex);
	};

	// Dragging logic
	const handleStopPointerDown = (e: React.PointerEvent, index: number) => {
		e.stopPropagation();
		e.preventDefault(); // Prevent selection
		e.currentTarget.setPointerCapture(e.pointerId);
		setDraggingStopIndex(index);
		setDragStartPos({ x: e.clientX, y: e.clientY });
	};

	const handleStopPointerMove = (e: React.PointerEvent) => {
		if (draggingStopIndex === null || !containerRef.current) return;
		e.stopPropagation();

		const rect = containerRef.current.getBoundingClientRect();
		const t = Math.max(0, Math.min(1, (e.clientX - rect.left) / rect.width));

		// Update stops locally for smooth preview
		const newStops = [...stops];
		newStops[draggingStopIndex].t = t;

		updateStops(newStops);
	};

	const handleStopPointerUp = (e: React.PointerEvent) => {
		if (draggingStopIndex === null) return;
		e.stopPropagation();
		e.currentTarget.releasePointerCapture(e.pointerId);

		// Detect if it was a drag or a click
		if (dragStartPos) {
			const dist = Math.abs(e.clientX - dragStartPos.x);
			if (dist < 3) {
				// It was a click
				setSelectedStopIndex(draggingStopIndex);
			}
		}

		setDraggingStopIndex(null);
		setDragStartPos(null);

		// Ensure final sort
		updateStops([...stops]);
	};

	// Keyboard delete
	React.useEffect(() => {
		const handleKeyDown = (e: KeyboardEvent) => {
			if (
				(e.key === "Delete" || e.key === "Backspace") &&
				selectedStopIndex !== null
			) {
				if (stops.length > 2) {
					const newStops = [...stops];
					newStops.splice(selectedStopIndex, 1);
					updateStops(newStops);
					setSelectedStopIndex(null);
				}
			}
		};
		window.addEventListener("keydown", handleKeyDown);
		return () => window.removeEventListener("keydown", handleKeyDown);
	}, [selectedStopIndex, stops, updateStops]);

	const controls = (
		// biome-ignore lint/a11y/noStaticElementInteractions: Interactive container
		<div
			className="flex flex-col gap-2 p-2 w-72 nodrag"
			onMouseDown={(e) => e.stopPropagation()}
		>
			<div className="text-xs text-slate-400 mb-1 flex justify-between">
				<span>Gradient</span>
				{draggingStopIndex !== null && (
					<span className="text-cyan-400">
						{Math.round(stops[draggingStopIndex].t * 100)}%
					</span>
				)}
			</div>

			<div className="relative h-10 mb-2" ref={containerRef}>
				{/* Track */}
				{/* biome-ignore lint/a11y/useKeyWithClickEvents: Interactive track */}
				{/* biome-ignore lint/a11y/noStaticElementInteractions: Interactive track */}
				<div
					className="absolute top-0 bottom-0 left-0 right-0 rounded border border-slate-600 cursor-crosshair overflow-hidden"
					style={{ background: gradientCss }}
					onClick={handleTrackClick}
				/>

				{/* Stops */}
				{stops.map((stop, i) => {
					const isSelected = selectedStopIndex === i;
					const isDragging = draggingStopIndex === i;

					const r = Math.round(stop.r * 255)
						.toString(16)
						.padStart(2, "0");
					const g = Math.round(stop.g * 255)
						.toString(16)
						.padStart(2, "0");
					const b = Math.round(stop.b * 255)
						.toString(16)
						.padStart(2, "0");
					const hex = `#${r}${g}${b}`;

					return (
						<Popover
							key={`${stop.t}-${i}`}
							open={selectedStopIndex === i}
							onOpenChange={(open) => {
								if (!open) setSelectedStopIndex(null);
							}}
						>
							<PopoverAnchor asChild>
								{/* Handle Visual */}
								<div
									className={`absolute top-0 bottom-0 w-3 -ml-1.5 cursor-grab active:cursor-grabbing transition-transform flex flex-col justify-center items-center ${
										isSelected ? "z-20 scale-110" : "z-10"
									}`}
									style={{
										left: `${stop.t * 100}%`,
									}}
									onPointerDown={(e) => handleStopPointerDown(e, i)}
									onPointerMove={isDragging ? handleStopPointerMove : undefined}
									onPointerUp={isDragging ? handleStopPointerUp : undefined}
								>
									{/* Vertical line indicator */}
									<div
										className={`w-1 h-full shadow-sm ${
											isSelected
												? "bg-white border-x border-black/20"
												: "bg-white/80 border-x border-black/10"
										}`}
									/>
									{/* Color dot at center to show color */}
									<div
										className={`absolute w-4 h-4 rounded-full border-2 shadow-sm ${
											isSelected
												? "border-white ring-1 ring-black/50"
												: "border-white/80"
										}`}
										style={{
											backgroundColor: `rgba(${stop.r * 255},${stop.g * 255},${stop.b * 255},${stop.a})`,
										}}
									/>
								</div>
							</PopoverAnchor>
							<PopoverContent className="w-auto p-3" side="top" sideOffset={10}>
								<div className="flex flex-col gap-3">
									<div className="flex items-center justify-between">
										<span className="text-xs font-bold">Edit Color</span>
										<Button
											variant="ghost"
											size="sm"
											className="h-6 w-6 p-0 text-red-400 hover:text-red-300"
											onClick={() => {
												const newStops = [...stops];
												newStops.splice(i, 1);
												updateStops(newStops);
												setSelectedStopIndex(null);
											}}
											disabled={stops.length <= 2}
										>
											<Trash2 className="h-4 w-4" />
										</Button>
									</div>

									<ColorPicker
										value={hex}
										onChange={(rgba) => {
											if (Array.isArray(rgba) && rgba.length >= 4) {
												const r = Number(rgba[0]) / 255;
												const g = Number(rgba[1]) / 255;
												const b = Number(rgba[2]) / 255;
												const a = Number(rgba[3]);

												if (
													Math.abs(r - stop.r) > 0.001 ||
													Math.abs(g - stop.g) > 0.001 ||
													Math.abs(b - stop.b) > 0.001 ||
													Math.abs(a - stop.a) > 0.001
												) {
													const newStops = [...stops];
													newStops[i] = { ...stop, r, g, b, a };
													updateStops(newStops);
												}
											}
										}}
										className="w-48"
									>
										<div className="flex flex-col gap-2">
											<ColorPickerSelection className="h-24 w-full rounded" />
											<ColorPickerHue />
											<ColorPickerAlpha />
										</div>
									</ColorPicker>
								</div>
							</PopoverContent>
						</Popover>
					);
				})}
			</div>

			<div className="text-[10px] text-slate-500 text-center">
				Click track to add • Drag stops to move
			</div>
		</div>
	);

	return <BaseNode {...props} data={{ ...data, paramControls: controls }} />;
}
