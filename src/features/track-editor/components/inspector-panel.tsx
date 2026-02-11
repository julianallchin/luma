import { CircleHelp } from "lucide-react";
import { useEffect, useState } from "react";
import type { BlendMode } from "@/bindings/schema";
import { useAppViewStore } from "@/features/app/stores/use-app-view-store";
import { TagExpressionEditor } from "@/features/universe/components/tag-expression-editor";
import {
	HoverCard,
	HoverCardContent,
	HoverCardTrigger,
} from "@/shared/components/ui/hover-card";
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

type ColorMode = "inherit" | "override" | "mix";
type RgbaValue = { r: number; g: number; b: number; a?: number };

type SelectionValue = { expression: string; spatialReference: string };

export function InspectorPanel() {
	const selectedAnnotationIds = useTrackEditorStore(
		(s) => s.selectedAnnotationIds,
	);
	const annotations = useTrackEditorStore((s) => s.annotations);
	const patternArgs = useTrackEditorStore((s) => s.patternArgs);
	const updateAnnotation = useTrackEditorStore((s) => s.updateAnnotation);
	const beatGrid = useTrackEditorStore((s) => s.beatGrid);
	const currentVenueId = useAppViewStore((s) => s.currentVenue?.id ?? null);

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
			<div className="w-80 border-l border-border bg-background/50 flex flex-col">
				<div className="p-3 border-b border-border/50 flex items-center">
					<h2 className="text-xs font-semibold text-muted-foreground uppercase tracking-wide">
						Inspector
					</h2>
				</div>
				<div className="flex-1 p-8 flex items-center justify-center text-muted-foreground text-sm">
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
			const val = value as RgbaValue;
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

	const parseColorMode = (value: unknown): ColorMode => {
		if (
			value &&
			typeof value === "object" &&
			"a" in value &&
			typeof (value as { a?: unknown }).a === "number"
		) {
			const a = (value as { a: number }).a;
			if (a <= 0) return "inherit";
			if (a >= 1) return "override";
			return "mix";
		}
		return "override";
	};

	const normalizeRgb = (value: unknown, fallback: unknown): RgbaValue => {
		const v =
			value && typeof value === "object" && "r" in value
				? (value as RgbaValue)
				: null;
		const f =
			fallback && typeof fallback === "object" && "r" in fallback
				? (fallback as RgbaValue)
				: { r: 255, g: 0, b: 0, a: 1 };

		return {
			r: Number(v?.r ?? f.r),
			g: Number(v?.g ?? f.g),
			b: Number(v?.b ?? f.b),
			a: typeof v?.a === "number" ? v.a : f.a,
		};
	};

	const setColorMode = (
		argId: string,
		nextMode: ColorMode,
		current: unknown,
		fallback: unknown,
	) => {
		const base = normalizeRgb(current, fallback);
		if (nextMode === "inherit") {
			handleArgChange(argId, { r: base.r, g: base.g, b: base.b, a: 0 });
			return;
		}
		if (nextMode === "override") {
			handleArgChange(argId, { r: base.r, g: base.g, b: base.b, a: 1 });
			return;
		}
		// mix
		const currentA = typeof base.a === "number" ? base.a : 1;
		const nextA = currentA > 0.0001 && currentA < 0.9999 ? currentA : 0.5;
		handleArgChange(argId, { r: base.r, g: base.g, b: base.b, a: nextA });
	};

	return (
		<div className="w-80 border-l border-border bg-background/50 flex flex-col">
			<div className="p-3 border-b border-border/50 flex items-center">
				<h2 className="text-xs font-semibold text-muted-foreground uppercase tracking-wide">
					Inspector
				</h2>
			</div>
			<div className="flex-1 p-4 space-y-6 overflow-y-auto">
				<div>
					<div className="text-xs font-semibold text-muted-foreground uppercase tracking-wider mb-3">
						Pattern
					</div>

					<div className="space-y-4">
						<div className="space-y-1">
							<div className="text-xs text-muted-foreground">Name</div>
							<div className="text-sm font-medium text-foreground/90 truncate">
								{selectedAnnotation.patternName ||
									`Pattern ${selectedAnnotation.patternId}`}
							</div>
						</div>
					</div>
				</div>

				<div className="h-px bg-border/50" />

				<div>
					<div className="text-xs font-semibold text-muted-foreground uppercase tracking-wider mb-3">
						Timing
					</div>

					<div className="space-y-4">
						<div className="grid grid-cols-2 gap-2">
							<div className="space-y-1">
								<label
									htmlFor="annotation-start-beat"
									className="text-xs text-muted-foreground"
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
									className="text-xs text-muted-foreground"
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
								className="text-xs text-muted-foreground"
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

				<div className="h-px bg-border/50" />

				<div>
					<div className="text-xs font-semibold text-muted-foreground uppercase tracking-wider mb-3">
						Pattern Args
					</div>

					{argsForPattern.length === 0 ? (
						<div className="text-xs text-muted-foreground">
							This pattern has no args.
						</div>
					) : (
						<div className="space-y-3">
							{argsForPattern.map((arg) => {
								const currentValue = (
									selectedAnnotation.args as Record<string, unknown> | undefined
								)?.[arg.id];
								const colorMode = parseColorMode(currentValue);
								const defaultColor =
									(arg.defaultValue as Record<string, unknown>) ?? {};
								const currentHex = parseColorHex(currentValue);

								if (arg.argType === "Color") {
									return (
										<div key={arg.id} className="space-y-1">
											<div className="flex items-center justify-between gap-2">
												<div className="text-xs text-muted-foreground">
													{arg.name}
												</div>
												<Select
													value={colorMode}
													onValueChange={(value) =>
														setColorMode(
															arg.id,
															value as ColorMode,
															currentValue,
															defaultColor,
														)
													}
												>
													<SelectTrigger className="h-7 w-28">
														<SelectValue />
													</SelectTrigger>
													<SelectContent>
														<SelectItem value="inherit">Inherit</SelectItem>
														<SelectItem value="override">Override</SelectItem>
														<SelectItem value="mix">Mix</SelectItem>
													</SelectContent>
												</Select>
											</div>
											<Popover
												open={openArgId === arg.id}
												onOpenChange={(open) =>
													setOpenArgId(open ? arg.id : null)
												}
											>
												<PopoverTrigger asChild>
													<button
														type="button"
														className="w-full h-7 flex items-center justify-between bg-input border border-border rounded-md overflow-hidden pr-2 text-sm text-foreground/90 hover:border-border transition-colors"
													>
														<div className="flex items-center gap-2 h-full">
															{colorMode === "inherit" ? (
																<>
																	<span className="w-7 self-stretch border-r border-border p-1">
																		<span className="w-full h-full rounded-sm bg-muted/50 border border-border block" />
																	</span>
																	<span className="text-xs text-muted-foreground">
																		Inherit
																	</span>
																</>
															) : (
																<>
																	<span className="w-7 self-stretch border-r border-border p-1">
																		<span
																			className="w-full h-full rounded-sm block"
																			style={{ backgroundColor: currentHex }}
																		/>
																	</span>
																	<span className="font-mono text-xs text-foreground/90">
																		{currentHex}
																	</span>
																</>
															)}
														</div>
														<span className="text-[10px] uppercase text-muted-foreground">
															Edit
														</span>
													</button>
												</PopoverTrigger>
												<PopoverContent className="w-auto bg-popover border border-border p-3">
													<ColorPicker
														defaultValue={currentHex}
														onChange={(rgba) => {
															if (Array.isArray(rgba) && rgba.length >= 4) {
																const base = normalizeRgb(
																	currentValue,
																	defaultColor,
																);
																const nextARaw =
																	colorMode === "inherit"
																		? 0
																		: colorMode === "override"
																			? 1
																			: Number(rgba[3]);
																const nextA =
																	colorMode === "mix"
																		? Math.min(
																				0.9999,
																				Math.max(0.0001, nextARaw),
																			)
																		: nextARaw;
																handleArgChange(arg.id, {
																	r:
																		colorMode === "inherit"
																			? base.r
																			: Math.round(Number(rgba[0])),
																	g:
																		colorMode === "inherit"
																			? base.g
																			: Math.round(Number(rgba[1])),
																	b:
																		colorMode === "inherit"
																			? base.b
																			: Math.round(Number(rgba[2])),
																	a: nextA,
																});
															}
														}}
													>
														<div className="flex flex-col gap-2">
															<ColorPickerSelection className="h-28 w-48 rounded-md" />
															<ColorPickerHue className="flex-1" />
															{colorMode === "mix" ? (
																<ColorPickerAlpha />
															) : null}
														</div>
													</ColorPicker>
												</PopoverContent>
											</Popover>
										</div>
									);
								}
								if (arg.argType === "Scalar") {
									const scalarValue =
										typeof currentValue === "number" ? currentValue : 1.0;
									return (
										<div key={arg.id} className="space-y-1">
											<div className="text-xs text-muted-foreground">
												{arg.name}
											</div>
											<Input
												type="number"
												step="0.1"
												value={scalarValue}
												onChange={(e) =>
													handleArgChange(arg.id, Number(e.target.value))
												}
												className="bg-input border-border text-sm"
											/>
										</div>
									);
								}
								if (arg.argType === "Selection") {
									const defaultSelection = (arg.defaultValue ?? {
										expression: "all",
										spatialReference: "global",
									}) as SelectionValue;
									const selectionValue = (currentValue ??
										defaultSelection) as SelectionValue;
									const expression = selectionValue.expression ?? "all";
									const spatialReference =
										selectionValue.spatialReference ?? "global";

									return (
										<div key={arg.id} className="space-y-2">
											<div className="flex items-center gap-1 text-xs text-muted-foreground">
												{arg.name}
												<HoverCard openDelay={200}>
													<HoverCardTrigger asChild>
														<button
															type="button"
															className="text-muted-foreground/60 hover:text-muted-foreground transition-colors"
														>
															<CircleHelp className="size-3" />
														</button>
													</HoverCardTrigger>
													<HoverCardContent
														side="left"
														align="start"
														className="w-96 text-xs space-y-2"
													>
														<p className="font-medium text-foreground">
															Tag Expressions
														</p>
														<p>
															Fixtures are organized into{" "}
															<span className="font-medium text-foreground">
																groups
															</span>
															, each with user-assigned{" "}
															<span className="font-medium text-foreground">
																tags
															</span>{" "}
															(e.g. <code className="text-amber-400">left</code>
															, <code className="text-amber-400">blinder</code>,{" "}
															<code className="text-amber-400">front</code>).
															Write expressions to select fixtures by their
															tags.
														</p>
														<div className="space-y-1">
															<p className="font-medium text-foreground">
																Operators
															</p>
															<div className="font-mono text-muted-foreground space-y-0.5">
																<div>
																	<code className="text-rose-400">|</code> union
																	(or)
																</div>
																<div>
																	<code className="text-rose-400">&</code>{" "}
																	intersection (and)
																</div>
																<div>
																	<code className="text-rose-400">~</code>{" "}
																	negate (not)
																</div>
																<div>
																	<code className="text-rose-400">^</code>{" "}
																	random choice (xor)
																</div>
																<div>
																	<code className="text-rose-400">{">"}</code>{" "}
																	fallback (if left empty, use right)
																</div>
																<div>
																	<code className="text-rose-400">( )</code>{" "}
																	grouping
																</div>
															</div>
														</div>
														<div className="space-y-1">
															<p className="font-medium text-foreground">
																Built-in tokens
															</p>
															<p className="font-mono text-muted-foreground">
																all, has_color, has_movement, has_strobe,
																moving_head, par_wash, pixel_bar, scanner,
																strobe
															</p>
														</div>
														<div className="space-y-1">
															<p className="font-medium text-foreground">
																Examples
															</p>
															<div className="font-mono text-muted-foreground space-y-0.5">
																<div className="flex justify-between">
																	<span>
																		<code className="text-amber-400">left</code>{" "}
																		<code className="text-rose-400">&</code>{" "}
																		<code className="text-amber-400">
																			moving_head
																		</code>
																	</span>{" "}
																	<span className="text-muted-foreground/60">
																		left movers
																	</span>
																</div>
																<div className="flex justify-between">
																	<span>
																		<code className="text-amber-400">
																			moving_head
																		</code>{" "}
																		<code className="text-rose-400">{">"}</code>{" "}
																		<code className="text-amber-400">
																			scanner
																		</code>
																	</span>{" "}
																	<span className="text-muted-foreground/60">
																		movers, else scanners
																	</span>
																</div>
																<div className="flex justify-between">
																	<span>
																		<code className="text-rose-400">~</code>
																		<code className="text-amber-400">
																			has_strobe
																		</code>
																	</span>{" "}
																	<span className="text-muted-foreground/60">
																		everything but strobes
																	</span>
																</div>
															</div>
														</div>
														<a
															href="https://luma.show/docs/architecture/selection-system"
															target="_blank"
															rel="noreferrer"
															className="text-blue-400 hover:underline"
														>
															Learn more
														</a>
													</HoverCardContent>
												</HoverCard>
											</div>
											<TagExpressionEditor
												value={expression}
												onChange={(newExpr) =>
													handleArgChange(arg.id, {
														expression: newExpr,
														spatialReference,
													})
												}
												venueId={currentVenueId}
											/>
											<Select
												value={spatialReference}
												onValueChange={(value) =>
													handleArgChange(arg.id, {
														expression,
														spatialReference: value,
													})
												}
											>
												<SelectTrigger className="w-full">
													<SelectValue />
												</SelectTrigger>
												<SelectContent>
													<SelectItem value="global">Global</SelectItem>
													<SelectItem value="group_local">
														Group Local
													</SelectItem>
												</SelectContent>
											</Select>
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
