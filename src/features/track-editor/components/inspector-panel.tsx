import { useEffect, useState } from "react";
import type { BlendMode } from "@/bindings/schema";
import {
	GradientPicker,
	type GradientStop,
} from "@/shared/components/gradient-picker";
import { Input } from "@/shared/components/ui/input";
import {
	Popover,
	PopoverContent,
	PopoverTrigger,
} from "@/shared/components/ui/popover";
import {
	Select,
	SelectContent,
	SelectItem,
	SelectTrigger,
	SelectValue,
} from "@/shared/components/ui/select";
import {
	ColorPicker,
	ColorPickerAlpha,
	ColorPickerHue,
	ColorPickerSelection,
} from "@/shared/components/ui/shadcn-io/color-picker";
import { useTrackEditorStore } from "../stores/use-track-editor-store";

export function InspectorPanel() {
	const selectedAnnotationIds = useTrackEditorStore(
		(s) => s.selectedAnnotationIds,
	);
	const annotations = useTrackEditorStore((s) => s.annotations);
	const patternArgs = useTrackEditorStore((s) => s.patternArgs);
	const updateAnnotation = useTrackEditorStore((s) => s.updateAnnotation);
	const beatGrid = useTrackEditorStore((s) => s.beatGrid);

	// For now, only show inspector for first selected annotation
	const selectedAnnotation = annotations.find((a) =>
		selectedAnnotationIds.includes(a.id),
	);

	// Local state for inputs to avoid stuttering while typing
	const [startBeat, setStartBeat] = useState("");
	const [endBeat, setEndBeat] = useState("");
	const [blendMode, setBlendMode] = useState<BlendMode>("replace");
	const [openArgId, setOpenArgId] = useState<string | null>(null);

	// Convert seconds to beats
	const secondsToBeats = (seconds: number): number => {
		if (!beatGrid || beatGrid.bpm === 0) return seconds;
		const beatLength = 60 / beatGrid.bpm;
		return (seconds - beatGrid.downbeatOffset) / beatLength;
	};

	// Convert beats to seconds
	const beatsToSeconds = (beats: number): number => {
		if (!beatGrid || beatGrid.bpm === 0) return beats;
		const beatLength = 60 / beatGrid.bpm;
		return beats * beatLength + beatGrid.downbeatOffset;
	};

	// Sync local state when selection changes
	useEffect(() => {
		if (selectedAnnotation) {
			setStartBeat(secondsToBeats(selectedAnnotation.startTime).toFixed(2));
			setEndBeat(secondsToBeats(selectedAnnotation.endTime).toFixed(2));
			setBlendMode(selectedAnnotation.blendMode || "replace");
		}
	}, [selectedAnnotation, beatGrid]);

	if (!selectedAnnotation) {
		return (
			<div className="w-80 border-l border-neutral-800 bg-neutral-900/50 flex flex-col">
				<div className="h-12 border-b border-neutral-800 flex items-center px-4 font-medium text-sm text-neutral-400">
					Inspector
				</div>
				<div className="flex-1 p-8 flex items-center justify-center text-neutral-500 text-sm">
					Select a pattern to view details
				</div>
			</div>
		);
	}

	const handleBlur = () => {
		const startBeats = parseFloat(startBeat);
		const endBeats = parseFloat(endBeat);

		if (!Number.isNaN(startBeats) && !Number.isNaN(endBeats)) {
			updateAnnotation({
				id: selectedAnnotation.id,
				startTime: beatsToSeconds(startBeats),
				endTime: beatsToSeconds(endBeats),
			});
		}
	};

	const handleBlendModeChange = (newBlendMode: BlendMode) => {
		setBlendMode(newBlendMode);
		updateAnnotation({
			id: selectedAnnotation.id,
			blendMode: newBlendMode,
		});
	};

	const argsForPattern = patternArgs[selectedAnnotation?.patternId ?? -1] ?? [];

	const handleArgChange = (
		argId: string,
		value: Record<string, unknown> | number,
	) => {
		if (!selectedAnnotation) return;
		const currentArgs =
			(selectedAnnotation.args as Record<string, unknown> | undefined) ?? {};
		const nextArgs = { ...currentArgs, [argId]: value };
		updateAnnotation({
			id: selectedAnnotation.id,
			args: nextArgs,
		});
	};

	const parseColorHex = (value: unknown) => {
		if (
			value &&
			typeof value === "object" &&
			"r" in value &&
			"g" in value &&
			"b" in value
		) {
			const val = value as { r: number; g: number; b: number; a?: number };
			const r = Math.round(Number(val.r) || 0)
				.toString(16)
				.padStart(2, "0");
			const g = Math.round(Number(val.g) || 0)
				.toString(16)
				.padStart(2, "0");
			const b = Math.round(Number(val.b) || 0)
				.toString(16)
				.padStart(2, "0");

			if ("a" in val && typeof val.a === "number") {
				const aVal = Math.max(0, Math.min(1, val.a));
				const a = Math.round(aVal * 255)
					.toString(16)
					.padStart(2, "0");
				return `#${r}${g}${b}${a}`;
			}
			return `#${r}${g}${b}`;
		}
		return "#ff0000";
	};

	return (
		<div className="w-80 border-l border-neutral-800 bg-neutral-900/50 flex flex-col">
			<div className="h-12 border-b border-neutral-800 flex items-center px-4 font-medium text-sm text-neutral-200">
				Inspector
			</div>
			<div className="flex-1 p-4 space-y-6 overflow-y-auto">
				<div>
					<div className="text-xs font-semibold text-neutral-500 uppercase tracking-wider mb-3">
						Pattern
					</div>

					<div className="space-y-4">
						<div className="space-y-1">
							<div className="text-xs text-neutral-400">Name</div>
							<div className="text-sm font-medium text-neutral-200 truncate">
								{selectedAnnotation.patternName ||
									`Pattern ${selectedAnnotation.patternId}`}
							</div>
						</div>
					</div>
				</div>

				<div className="h-px bg-neutral-800" />

				<div>
					<div className="text-xs font-semibold text-neutral-500 uppercase tracking-wider mb-3">
						Timing
					</div>

					<div className="space-y-4">
						<div className="grid grid-cols-2 gap-2">
							<div className="space-y-1">
								<label
									htmlFor="annotation-start-beat"
									className="text-xs text-neutral-400"
								>
									Start
								</label>
								<Input
									id="annotation-start-beat"
									type="text"
									value={startBeat}
									onChange={(e) => setStartBeat(e.target.value)}
									onBlur={handleBlur}
									onKeyDown={(e) => e.key === "Enter" && handleBlur()}
								/>
							</div>
							<div className="space-y-1">
								<label
									htmlFor="annotation-end-beat"
									className="text-xs text-neutral-400"
								>
									End
								</label>
								<Input
									id="annotation-end-beat"
									type="text"
									value={endBeat}
									onChange={(e) => setEndBeat(e.target.value)}
									onBlur={handleBlur}
									onKeyDown={(e) => e.key === "Enter" && handleBlur()}
								/>
							</div>
						</div>

						<div className="space-y-1">
							<label
								htmlFor="annotation-blend-mode"
								className="text-xs text-neutral-400"
							>
								Blend Mode
							</label>
							<Select
								value={blendMode}
								onValueChange={(value) =>
									handleBlendModeChange(value as BlendMode)
								}
							>
								<SelectTrigger id="annotation-blend-mode" className="w-full">
									<SelectValue />
								</SelectTrigger>
								<SelectContent>
									<SelectItem value="replace">Replace</SelectItem>
									<SelectItem value="add">Add</SelectItem>
									<SelectItem value="multiply">Multiply</SelectItem>
									<SelectItem value="screen">Screen</SelectItem>
									<SelectItem value="max">Max</SelectItem>
									<SelectItem value="min">Min</SelectItem>
									<SelectItem value="lighten">Lighten</SelectItem>
									<SelectItem value="value">Value</SelectItem>
								</SelectContent>
							</Select>
						</div>
					</div>
				</div>

				<div className="h-px bg-neutral-800" />

				<div>
					<div className="text-xs font-semibold text-neutral-500 uppercase tracking-wider mb-3">
						Pattern Args
					</div>

					{argsForPattern.length === 0 ? (
						<div className="text-xs text-neutral-500">
							This pattern has no args.
						</div>
					) : (
						<div className="space-y-3">
							{argsForPattern.map((arg) => {
								const currentValue = (
									selectedAnnotation.args as Record<string, unknown> | undefined
								)?.[arg.id];
								const currentHex = parseColorHex(currentValue);

								if (arg.argType === "Color") {
									return (
										<div key={arg.id} className="space-y-1">
											<div className="text-xs text-neutral-400">{arg.name}</div>
											<Popover
												open={openArgId === arg.id}
												onOpenChange={(open) =>
													setOpenArgId(open ? arg.id : null)
												}
											>
												<PopoverTrigger asChild>
													<button
														type="button"
														className="w-full flex items-center justify-between bg-neutral-950 border border-neutral-800 rounded px-2 py-2 text-sm text-neutral-200 hover:border-neutral-600"
													>
														<div className="flex items-center gap-2">
															<span
																className="w-5 h-5 rounded border border-neutral-700"
																style={{ backgroundColor: currentHex }}
															/>
															<span className="font-mono text-xs">
																{currentHex}
															</span>
														</div>
														<span className="text-[10px] uppercase text-neutral-500">
															Edit
														</span>
													</button>
												</PopoverTrigger>
												<PopoverContent className="w-auto bg-neutral-900 border border-neutral-800 p-3">
													<ColorPicker
														defaultValue={currentHex}
														onChange={(rgba) => {
															if (Array.isArray(rgba) && rgba.length >= 4) {
																handleArgChange(arg.id, {
																	r: Math.round(Number(rgba[0])),
																	g: Math.round(Number(rgba[1])),
																	b: Math.round(Number(rgba[2])),
																	a: Number(rgba[3]),
																});
															}
														}}
													>
														<div className="flex flex-col gap-2">
															<ColorPickerSelection className="h-28 w-48 rounded" />
															<ColorPickerHue className="flex-1" />
															<ColorPickerAlpha />
														</div>
													</ColorPicker>
												</PopoverContent>
											</Popover>
										</div>
									);
								}
								if (arg.argType === "Gradient") {
									const gradientValue = (currentValue as {
										stops?: GradientStop[];
										mode?: string;
									}) || { stops: [], mode: "linear" };
									const stops = gradientValue.stops || [];

									return (
										<div key={arg.id} className="space-y-1">
											<div className="text-xs text-neutral-400">{arg.name}</div>
											<GradientPicker
												value={stops}
												onChange={(newStops) =>
													handleArgChange(arg.id, {
														...gradientValue,
														stops: newStops,
													})
												}
												className="bg-neutral-950 border border-neutral-800 rounded p-2"
											/>
										</div>
									);
								}
								if (arg.argType === "Scalar") {
									const scalarValue =
										typeof currentValue === "number" ? currentValue : 1.0;
									return (
										<div key={arg.id} className="space-y-1">
											<div className="text-xs text-neutral-400">{arg.name}</div>
											<Input
												type="number"
												step="0.1"
												value={scalarValue}
												onChange={(e) =>
													handleArgChange(arg.id, Number(e.target.value))
												}
												className="bg-neutral-950 border-neutral-800 text-sm"
											/>
										</div>
									);
								}
								return null;
							})}
						</div>
					)}
				</div>
			</div>
		</div>
	);
}
