import { convertFileSrc } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
	GitFork,
	Globe,
	GlobeLock,
	Layers,
	Pause,
	Pencil,
	Play,
	RefreshCw,
	Repeat,
	Save,
	SkipBack,
	Trash2,
	X,
} from "lucide-react";
import {
	useCallback,
	useEffect,
	useLayoutEffect,
	useMemo,
	useRef,
	useState,
} from "react";
import { useBlocker, useLocation, useNavigate } from "react-router-dom";
import { toast } from "sonner";
import type { FixtureGroupNode } from "@/bindings/groups";
import type {
	AnnotationPreview,
	BeatGrid,
	Graph,
	GraphContext,
	GraphEditResult,
	HostAudioSnapshot,
	MelSpec,
	NodeTypeDef,
	PatternArgDef,
	PatternArgType,
	PatternCategory,
	PatternSummary,
	RunResult as SchemaRunResult,
	Signal,
	TrackSummary,
} from "@/bindings/schema";
import { useAppViewStore } from "@/features/app/stores/use-app-view-store";
import { useAuthStore } from "@/features/auth/stores/use-auth-store";
import { graphAgent } from "@/features/patterns/agent/graph-agent";
import {
	checkpointGraphDocument,
	graphFingerprint,
} from "@/features/patterns/agent/graph-checkpoint";
import { layoutGraph } from "@/features/patterns/agent/graph-layout";
import { withPatternArgsNode } from "@/features/patterns/agent/graph-tools";
import { GraphAgentPanel } from "@/features/patterns/components/graph-agent-panel";
import {
	type PatternAnnotationInstance,
	PatternAnnotationProvider,
} from "@/features/patterns/contexts/pattern-annotation-context";
import { useGraphStore } from "@/features/patterns/stores/use-graph-store";
import {
	getExtrapolatedHostTime,
	useHostAudioStore,
} from "@/features/patterns/stores/use-host-audio-store";
import { usePatternsStore } from "@/features/patterns/stores/use-patterns-store";
import { resetViewDataStore } from "@/features/patterns/stores/use-view-data-store";
import type {
	TrackScore,
	TrackWaveform,
} from "@/features/track-editor/stores/use-track-editor-store";
import { GroupExpressionEditor } from "@/features/universe/components/group-expression-editor";
import { useFixtureStore } from "@/features/universe/stores/use-fixture-store";
import { StageVisualizer } from "@/features/visualizer/components/stage-visualizer";
import {
	GradientStops,
	type GradientValue,
	PaletteSwatches,
	type PaletteValue,
} from "@/shared/components/palette-editor";
import {
	AlertDialog,
	AlertDialogAction,
	AlertDialogCancel,
	AlertDialogContent,
	AlertDialogDescription,
	AlertDialogFooter,
	AlertDialogHeader,
	AlertDialogTitle,
} from "@/shared/components/ui/alert-dialog";
import { Button } from "@/shared/components/ui/button";
import {
	Dialog,
	DialogContent,
	DialogDescription,
	DialogFooter,
	DialogHeader,
	DialogTitle,
} from "@/shared/components/ui/dialog";
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
	ColorPickerCopyPaste,
	ColorPickerHue,
	ColorPickerSelection,
} from "@/shared/components/ui/shadcn-io/color-picker";
import { Slider } from "@/shared/components/ui/slider";
import { Textarea } from "@/shared/components/ui/textarea";
import { Toggle } from "@/shared/components/ui/toggle";
import { IdempotentRequestGate } from "@/shared/lib/idempotent-request";
import { LatestRequestGate } from "@/shared/lib/latest-request-gate";
import { formatTime } from "@/shared/lib/react-flow/base-node";
import { patternArgsNodeDef as patternArgsNodeDefFor } from "@/shared/lib/react-flow/pattern-args-node-def";
import {
	type EditorController,
	ReactFlowEditorWrapper,
} from "@/shared/lib/react-flow-editor";
import { invoke } from "@/shared/lib/tauri";
import { toSnakeCase } from "@/shared/lib/utils";

type RunResult = {
	views: Record<string, Signal>;
	melSpecs: Record<string, MelSpec>;
	colorViews: Record<string, string>;
	universeState?: unknown;
};

type GraphContextWithSeed = GraphContext & { instanceSeed?: number };

const generateSelectionSeed = () =>
	Math.floor(Math.random() * Number.MAX_SAFE_INTEGER);

/** Build the `argValues` for a graph run: pattern defaults, overlaid with the
 * instance's saved args, then — if a preview-only selection is active — every
 * Selection arg's expression is overridden (preview always wins; the saved
 * pattern is untouched). */
function buildRunArgValues(
	patternArgs: PatternArgDef[],
	instanceArgs: Record<string, unknown>,
	previewSelection: string | null,
): Record<string, unknown> {
	const out: Record<string, unknown> = {};
	for (const arg of patternArgs) out[arg.id] = arg.defaultValue ?? {};
	Object.assign(out, instanceArgs);
	if (previewSelection) {
		for (const arg of patternArgs) {
			if (arg.argType === "Selection") {
				const base = (out[arg.id] as Record<string, unknown>) ?? {};
				out[arg.id] = { ...base, expression: previewSelection };
			}
		}
	}
	return out;
}

/** Override Selection args' expression in an args array (for the preview-image
 * backend, which derives its arg values from the graph's args). */
function overrideSelectionArgs(
	args: PatternArgDef[],
	previewSelection: string | null,
): PatternArgDef[] {
	if (!previewSelection) return args;
	return args.map((arg) =>
		arg.argType === "Selection"
			? {
					...arg,
					defaultValue: {
						...(arg.defaultValue ?? {}),
						expression: previewSelection,
					},
				}
			: arg,
	);
}

function computeBarRangeLabel(
	start: number,
	end: number,
	beatGrid: BeatGrid | null,
): string {
	if (!beatGrid) return "Bars —";
	const barDuration = (60 / beatGrid.bpm) * beatGrid.beatsPerBar;
	const offset = beatGrid.downbeatOffset ?? 0;
	const startBar = Math.max(1, Math.floor((start - offset) / barDuration) + 1);
	const endBar = Math.max(
		startBar,
		Math.floor((end - offset) / barDuration) + 1,
	);
	return `Bars ${startBar}–${endBar}`;
}

type MiniWaveformPreviewProps = {
	waveform: TrackWaveform | null;
	startTime: number;
	endTime: number;
};

function MiniWaveformPreview({
	waveform,
	startTime,
	endTime,
}: MiniWaveformPreviewProps) {
	const canvasRef = useRef<HTMLCanvasElement | null>(null);

	useEffect(() => {
		const canvas = canvasRef.current;
		if (!canvas) return;
		const ctx = canvas.getContext("2d");
		if (!ctx) return;

		const width = canvas.clientWidth || 240;
		const height = canvas.clientHeight || 56;
		const dpr = window.devicePixelRatio || 1;
		if (canvas.width !== width * dpr || canvas.height !== height * dpr) {
			canvas.width = width * dpr;
			canvas.height = height * dpr;
			canvas.style.width = `${width}px`;
			canvas.style.height = `${height}px`;
			ctx.scale(dpr, dpr);
		}

		ctx.clearRect(0, 0, width, height);

		const totalDuration = waveform?.durationSeconds ?? 0;
		const windowStart = Math.max(0, startTime);
		const windowEnd =
			endTime > 0 ? Math.min(endTime, totalDuration) : totalDuration;
		const windowDuration = Math.max(0.001, windowEnd - windowStart);

		const BLUE = [0, 85, 226];
		const ORANGE = [242, 170, 60];
		const WHITE = [255, 255, 255];

		if (waveform?.bands) {
			const { low, mid, high } = waveform.bands;
			const numBuckets = low.length;
			for (let x = 0; x < width; x += 1) {
				const t = windowStart + (windowDuration * x) / width;
				const bucketIdx = Math.min(
					numBuckets - 1,
					Math.floor((t / totalDuration) * numBuckets),
				);
				const lowH = Math.floor(low[bucketIdx] * (height / 2));
				const midH = Math.floor(mid[bucketIdx] * (height / 2));
				const highH = Math.floor(high[bucketIdx] * (height / 2));
				const centerY = height / 2;

				if (lowH > 0) {
					ctx.fillStyle = `rgb(${BLUE[0]}, ${BLUE[1]}, ${BLUE[2]})`;
					ctx.fillRect(x, centerY - lowH, 1, lowH * 2);
				}
				if (midH > 0) {
					ctx.fillStyle = `rgb(${ORANGE[0]}, ${ORANGE[1]}, ${ORANGE[2]})`;
					ctx.fillRect(x, centerY - midH, 1, midH * 2);
				}
				if (highH > 0) {
					ctx.fillStyle = `rgb(${WHITE[0]}, ${WHITE[1]}, ${WHITE[2]})`;
					ctx.fillRect(x, centerY - highH, 1, highH * 2);
				}
			}
		} else if (waveform?.fullSamples?.length) {
			const samples = waveform.fullSamples;
			const numBuckets = samples.length / 2;
			ctx.fillStyle = "rgba(94, 234, 212, 0.6)";
			for (let x = 0; x < width; x += 1) {
				const t = windowStart + (windowDuration * x) / width;
				const bucketIndex = Math.floor((t / totalDuration) * numBuckets) * 2;
				const min = samples[bucketIndex] ?? 0;
				const max = samples[bucketIndex + 1] ?? 0;
				const yTop = height / 2 - max * (height / 2) * 0.9;
				const yBottom = height / 2 - min * (height / 2) * 0.9;
				const h = Math.abs(yBottom - yTop) || 1;
				ctx.fillRect(x, Math.min(yTop, yBottom), 1, h);
			}
		} else {
			ctx.fillStyle = "rgba(255,255,255,0.05)";
			for (let i = 0; i < width; i += 6) {
				const h = (Math.sin(i / 10) * 0.5 + 0.5) * height * 0.3 + 8;
				ctx.fillRect(i, height / 2 - h / 2, 3, h);
			}
		}
	}, [waveform, startTime, endTime]);

	return <canvas ref={canvasRef} className="w-full h-8 bg-transparent" />;
}

type ContextSheetProps = {
	instances: PatternAnnotationInstance[];
	loading: boolean;
	error: string | null;
	selectedId: string | null;
	open: boolean;
	onSelect: (id: string) => void;
	onReload: () => void;
	onClose: () => void;
};

function ContextSheet({
	instances,
	loading,
	error,
	selectedId,
	open,
	onSelect,
	onReload,
	onClose,
}: ContextSheetProps) {
	return (
		<aside
			className={`absolute inset-y-0 left-0 z-40 w-72 bg-background border-r border-border flex flex-col transition-transform duration-200 ease-in-out ${
				open ? "translate-x-0" : "-translate-x-full"
			}`}
		>
			<div className="px-3 py-2 border-b border-border flex items-center justify-between bg-background">
				<p className="text-[11px] font-semibold uppercase tracking-wide text-foreground">
					Context
				</p>
				<div className="flex items-center gap-2">
					<button
						type="button"
						onClick={onReload}
						disabled={loading}
						className="text-[10px] text-muted-foreground hover:text-foreground disabled:opacity-50"
					>
						Refresh
					</button>
					<button
						type="button"
						onClick={onClose}
						className="text-muted-foreground hover:text-foreground"
					>
						<X size={14} />
					</button>
				</div>
			</div>
			<div className="flex-1 overflow-y-auto p-2 space-y-1.5">
				{error ? <div className="text-xs text-destructive">{error}</div> : null}
				{instances.map((instance) => {
					const isActive = instance.id === selectedId;
					const barLabel = computeBarRangeLabel(
						instance.startTime,
						instance.endTime,
						instance.beatGrid,
					);
					const timeLabel = `${formatTime(instance.startTime)} – ${formatTime(
						instance.endTime,
					)}`;
					return (
						<button
							type="button"
							key={instance.id}
							onClick={() => onSelect(instance.id)}
							className={`w-full text-left rounded border transition-colors ${
								isActive
									? "border-primary/70 bg-primary/10"
									: "border-border/60 bg-input hover:border-border hover:bg-muted shadow"
							}`}
						>
							<div className="px-2 py-1.5 flex items-center gap-2">
								{instance.track.albumArtPath ? (
									<img
										src={convertFileSrc(instance.track.albumArtPath)}
										alt=""
										loading="lazy"
										decoding="async"
										className="h-8 w-8 object-cover bg-muted/50 rounded-sm"
									/>
								) : (
									<div className="h-8 w-8 bg-muted/60 rounded-sm" />
								)}
								<div className="min-w-0 flex-1">
									<div className="flex items-center justify-between gap-1">
										<span className="font-medium truncate text-[11px] text-foreground">
											{instance.track.title ?? `Track ${instance.track.id}`}
										</span>
										<span className="text-[9px] text-muted-foreground whitespace-nowrap">
											{barLabel}
										</span>
									</div>
									<div className="text-[9px] text-muted-foreground">
										{timeLabel}
									</div>
								</div>
							</div>
							<MiniWaveformPreview
								waveform={instance.waveform}
								startTime={instance.startTime}
								endTime={instance.endTime}
							/>
						</button>
					);
				})}
				<p className="text-[10px] text-muted-foreground text-center py-2">
					Add this pattern to a track to see it here
				</p>
			</div>
		</aside>
	);
}

function sliceBeatGrid(grid: BeatGrid | null, _start: number, _end: number) {
	// Pass the full beat grid to the backend. Slicing it causes beat_envelope
	// pulse generation to lose phase alignment (subdivision/offset depend on
	// the beat's position in the array, not its absolute time).
	return grid;
}

type PatternInfoPanelProps = {
	pattern: PatternSummary | null;
	loading: boolean;
	args: PatternArgDef[];
	readOnly?: boolean;
	onAddArg: () => void;
	onEditArg: (arg: PatternArgDef) => void;
	onDeleteArg: (argId: string) => void;
	onRename: (name: string) => void;
	onUpdateDescription: (description: string | null) => void;
	onSetCategory: (categoryName: string | null) => void;
	onPublish?: (publish: boolean) => void;
};

function PatternInfoPanel({
	pattern,
	loading,
	args,
	readOnly,
	onAddArg,
	onEditArg,
	onDeleteArg,
	onRename,
	onUpdateDescription,
	onSetCategory,
	onPublish,
}: PatternInfoPanelProps) {
	const [isEditingName, setIsEditingName] = useState(false);
	const [editedName, setEditedName] = useState("");
	const [isEditingDescription, setIsEditingDescription] = useState(false);
	const [editedDescription, setEditedDescription] = useState("");
	const [categories, setCategories] = useState<PatternCategory[]>([]);
	const nameInputRef = useRef<HTMLInputElement>(null);
	const descriptionInputRef = useRef<HTMLTextAreaElement>(null);
	const normalizedName = toSnakeCase(editedName);

	useEffect(() => {
		invoke<PatternCategory[]>("list_pattern_categories")
			.then(setCategories)
			.catch((err) => console.error("Failed to load categories", err));
	}, []);

	const handleStartEditingName = () => {
		if (!pattern) return;
		setEditedName(pattern.name);
		setIsEditingName(true);
	};

	const handleSaveName = () => {
		if (normalizedName && normalizedName !== pattern?.name) {
			onRename(normalizedName);
		}
		setIsEditingName(false);
	};

	const handleNameKeyDown = (e: React.KeyboardEvent) => {
		if (e.key === "Enter" && normalizedName) {
			handleSaveName();
		} else if (e.key === "Escape") {
			setIsEditingName(false);
		}
	};

	const handleStartEditingDescription = () => {
		if (!pattern) return;
		setEditedDescription(pattern.description ?? "");
		setIsEditingDescription(true);
	};

	const handleSaveDescription = () => {
		const trimmed = editedDescription.trim();
		if (trimmed !== (pattern?.description ?? "")) {
			onUpdateDescription(trimmed || null);
		}
		setIsEditingDescription(false);
	};

	const handleDescriptionKeyDown = (e: React.KeyboardEvent) => {
		if (e.key === "Enter" && e.metaKey) {
			handleSaveDescription();
		} else if (e.key === "Escape") {
			setIsEditingDescription(false);
		}
	};

	useEffect(() => {
		if (isEditingName && nameInputRef.current) {
			nameInputRef.current.focus();
			nameInputRef.current.select();
		}
	}, [isEditingName]);

	useEffect(() => {
		if (isEditingDescription && descriptionInputRef.current) {
			descriptionInputRef.current.focus();
			descriptionInputRef.current.select();
		}
	}, [isEditingDescription]);

	if (loading) {
		return (
			<div className="w-full h-full bg-background flex flex-col">
				<div className="p-4 space-y-3">
					<div className="h-4 w-full bg-muted animate-pulse rounded" />
					<div className="h-4 w-3/4 bg-muted animate-pulse rounded" />
				</div>
			</div>
		);
	}

	if (!pattern) {
		return (
			<div className="w-full h-full bg-background flex flex-col">
				<div className="p-4 text-sm text-muted-foreground">
					Pattern not found
				</div>
			</div>
		);
	}

	return (
		<div className="w-full h-full bg-background flex flex-col">
			<div className="p-4 space-y-4 flex-1 min-h-0 overflow-y-auto">
				{/* Author attribution for community patterns */}
				{readOnly && pattern.authorName && (
					<div className="text-xs text-muted-foreground">
						by {pattern.authorName}
					</div>
				)}

				<div>
					<span className="text-[10px] uppercase tracking-wide text-muted-foreground font-medium">
						Name
					</span>
					{!readOnly && isEditingName ? (
						<div className="mt-0.5">
							<Input
								ref={nameInputRef}
								autoCapitalize="off"
								value={editedName}
								onChange={(e) => setEditedName(e.target.value)}
								onBlur={handleSaveName}
								onKeyDown={handleNameKeyDown}
								placeholder="my_pattern_name"
							/>
							{editedName && editedName !== normalizedName && (
								<p className="text-[10px] text-muted-foreground mt-1">
									{normalizedName ? (
										<>
											Will be saved as:{" "}
											<code className="bg-muted px-1 rounded">
												{normalizedName}
											</code>
										</>
									) : (
										<span className="text-destructive">
											Name must contain at least one letter or number
										</span>
									)}
								</p>
							)}
						</div>
					) : readOnly ? (
						<h2 className="text-lg font-semibold text-foreground mt-0.5">
							{pattern.name}
						</h2>
					) : (
						<button
							type="button"
							onClick={handleStartEditingName}
							className="w-full text-left group"
						>
							<h2 className="text-lg font-semibold text-foreground mt-0.5 group-hover:text-primary transition-colors flex items-center gap-2">
								{pattern.name}
								<Pencil
									size={14}
									className="opacity-0 group-hover:opacity-50 transition-opacity"
								/>
							</h2>
						</button>
					)}
				</div>

				<div>
					<span className="text-[10px] uppercase tracking-wide text-muted-foreground font-medium">
						Description
					</span>
					{!readOnly && isEditingDescription ? (
						<div className="mt-0.5">
							<Textarea
								ref={descriptionInputRef}
								value={editedDescription}
								onChange={(e) => setEditedDescription(e.target.value)}
								onBlur={handleSaveDescription}
								onKeyDown={handleDescriptionKeyDown}
								placeholder="Optional description"
								rows={3}
							/>
							<p className="text-[10px] text-muted-foreground mt-1">
								Press ⌘+Enter to save, Escape to cancel
							</p>
						</div>
					) : readOnly ? (
						<p className="text-sm text-foreground/80 mt-0.5 leading-relaxed">
							{pattern.description || (
								<span className="text-muted-foreground italic">
									No description
								</span>
							)}
						</p>
					) : (
						<button
							type="button"
							onClick={handleStartEditingDescription}
							className="w-full text-left group"
						>
							<p className="text-sm text-foreground/80 mt-0.5 leading-relaxed group-hover:text-primary transition-colors flex items-start gap-2">
								{pattern.description || (
									<span className="text-muted-foreground italic">
										No description provided
									</span>
								)}
								<Pencil
									size={12}
									className="opacity-0 group-hover:opacity-50 transition-opacity shrink-0 mt-1"
								/>
							</p>
						</button>
					)}
				</div>

				{categories.length > 0 && (
					<div>
						<span className="text-[10px] uppercase tracking-wide text-muted-foreground font-medium">
							Category
						</span>
						{readOnly ? (
							<p className="text-sm text-foreground/80 mt-0.5">
								{pattern.categoryName || (
									<span className="text-muted-foreground italic">None</span>
								)}
							</p>
						) : (
							<div className="mt-0.5">
								<Select
									value={pattern.categoryName ?? "none"}
									onValueChange={(v) => onSetCategory(v === "none" ? null : v)}
								>
									<SelectTrigger className="w-full">
										<SelectValue placeholder="None" />
									</SelectTrigger>
									<SelectContent>
										<SelectItem value="none">None</SelectItem>
										{categories.map((cat) => (
											<SelectItem key={cat.id} value={cat.name}>
												{cat.name}
											</SelectItem>
										))}
									</SelectContent>
								</Select>
							</div>
						)}
					</div>
				)}

				{/* Verify toggle (owner only) */}
				{!readOnly && onPublish && (
					<div className="pt-2 border-t border-border">
						<button
							type="button"
							onClick={() => onPublish(!pattern.isVerified)}
							className="flex items-center gap-2 text-xs text-muted-foreground hover:text-foreground transition-colors"
						>
							{pattern.isVerified ? (
								<>
									<Globe size={14} className="text-primary" />
									<span>Verified</span>
									<span className="text-[10px] text-muted-foreground/60 ml-1">
										(click to unverify)
									</span>
								</>
							) : (
								<>
									<GlobeLock size={14} />
									<span>Mark as verified</span>
								</>
							)}
						</button>
					</div>
				)}

				<div className="pt-2 border-t border-border">
					<div className="flex items-center justify-between">
						<span className="text-[10px] uppercase tracking-wide text-muted-foreground font-medium">
							Args
						</span>
						{!readOnly && (
							<button
								type="button"
								onClick={onAddArg}
								className="text-xs text-primary hover:underline"
							>
								Add Arg
							</button>
						)}
					</div>
					{args.length === 0 ? (
						<p className="text-sm text-muted-foreground mt-1">No args yet</p>
					) : (
						<div className="mt-2 space-y-2">
							{args.map((arg) => (
								<div
									key={arg.id}
									className="flex items-center justify-between text-sm group"
								>
									<div className="flex flex-col">
										<span className="text-foreground">{arg.name}</span>
										<span className="text-[11px] text-muted-foreground uppercase">
											{arg.argType}
										</span>
									</div>
									{!readOnly && (
										<div className="flex items-center gap-1 opacity-0 group-hover:opacity-100 transition-opacity">
											<button
												type="button"
												onClick={() => onEditArg(arg)}
												className="p-1.5 text-muted-foreground hover:text-foreground hover:bg-muted rounded"
												title="Edit argument"
											>
												<Pencil size={12} />
											</button>
											<button
												type="button"
												onClick={() => onDeleteArg(arg.id)}
												className="p-1.5 text-muted-foreground hover:text-destructive hover:bg-red-500/10 rounded"
												title="Delete argument"
											>
												<Trash2 size={12} />
											</button>
										</div>
									)}
								</div>
							))}
						</div>
					)}
				</div>

				<div className="pt-2 border-t border-border">
					<div className="flex items-center justify-between text-[10px] text-muted-foreground">
						<span>Created</span>
						<span>
							{new Date(pattern.createdAt).toLocaleDateString(undefined, {
								year: "numeric",
								month: "short",
								day: "numeric",
							})}
						</span>
					</div>
					<div className="flex items-center justify-between text-[10px] text-muted-foreground mt-1">
						<span>Updated</span>
						<span>
							{new Date(pattern.updatedAt).toLocaleDateString(undefined, {
								year: "numeric",
								month: "short",
								day: "numeric",
							})}
						</span>
					</div>
				</div>
			</div>
		</div>
	);
}

function TransportBar() {
	const isPlaying = useHostAudioStore((s) => s.isPlaying);
	const currentTime = useHostAudioStore((s) => s.currentTime);
	const durationSeconds = useHostAudioStore((s) => s.durationSeconds);
	const loopEnabled = useHostAudioStore((s) => s.loopEnabled);
	const [extrapolated, setExtrapolated] = useState(currentTime);

	// While playing, snapshots arrive at only a few Hz — reading currentTime
	// directly makes the thumb step. Extrapolate per frame off the same shared
	// clock the view-signal node playheads use, so they move in lockstep.
	useLayoutEffect(() => {
		if (!isPlaying) return;
		// Seed before paint so the first playing frame uses the scrubbed position,
		// not the stale extrapolated value from the previous play session.
		setExtrapolated(getExtrapolatedHostTime());
		let raf = requestAnimationFrame(function tick() {
			setExtrapolated(getExtrapolatedHostTime());
			raf = requestAnimationFrame(tick);
		});
		return () => cancelAnimationFrame(raf);
	}, [isPlaying]);

	const displayTime = isPlaying ? extrapolated : currentTime;
	const total = Math.max(durationSeconds, 0.0001);

	const handleSeek = async (value: number) => {
		await useHostAudioStore.getState().seek(value);
	};

	const handlePlayPause = async () => {
		const hostAudio = useHostAudioStore.getState();
		if (hostAudio.isPlaying) {
			await hostAudio.pause();
		} else if (hostAudio.isLoaded) {
			// If at the end, seek to start before playing
			if (hostAudio.currentTime >= hostAudio.durationSeconds - 0.05) {
				await hostAudio.seek(0);
			}
			await hostAudio.play();
		}
	};

	return (
		<div className="flex items-center gap-3 px-3 py-2 bg-card">
			{/* Controls */}
			<div className="flex items-center gap-1.5">
				<button
					type="button"
					onClick={() => handleSeek(0)}
					className="w-5 h-5 flex items-center justify-center text-muted-foreground hover:text-foreground hover:bg-hover transition-colors"
				>
					<SkipBack size={12} />
				</button>
				<button
					type="button"
					onClick={handlePlayPause}
					className="w-5 h-5 flex items-center justify-center text-foreground hover:bg-hover transition-colors"
				>
					{isPlaying ? (
						<Pause size={12} fill="currentColor" />
					) : (
						<Play size={12} fill="currentColor" />
					)}
				</button>
				<button
					type="button"
					className={`w-5 h-5 flex items-center justify-center transition-colors ${
						loopEnabled
							? "text-primary"
							: "text-muted-foreground hover:text-foreground hover:bg-hover"
					}`}
					title="Toggle Loop"
					onClick={() => useHostAudioStore.getState().setLoop(!loopEnabled)}
				>
					<Repeat size={12} />
				</button>
			</div>

			{/* Scrub Bar */}
			<span className="text-[10px] text-muted-foreground tabular-nums w-8 text-right">
				{formatTime(displayTime)}
			</span>
			<Slider
				aria-label="Playback position"
				className="flex-1"
				hideValue
				min={0}
				max={total}
				step="any"
				value={displayTime}
				onChange={(e) => {
					useHostAudioStore.getState().scrub(Number(e.target.value));
				}}
				onPointerDown={() => useHostAudioStore.getState().setScrubbing(true)}
				onPointerUp={() => useHostAudioStore.getState().setScrubbing(false)}
				onKeyDown={() => useHostAudioStore.getState().setScrubbing(true)}
				onKeyUp={() => useHostAudioStore.getState().setScrubbing(false)}
			/>
			<span className="text-[10px] text-muted-foreground tabular-nums w-8">
				{formatTime(durationSeconds)}
			</span>
		</div>
	);
}

/** Preview-only fixture selection. Patterns always select `all`; this bar lets
 * the user (and, via set_preview_selection, the agent) restrict the
 * preview/visualizer to specific venue groups. Toggling groups OR's them into
 * the tag expression stored on the graph store. */
function PreviewSelectionBar({ venueId }: { venueId: string | null }) {
	const [groups, setGroups] = useState<string[]>([]);
	const previewSelection = useGraphStore((s) => s.previewSelection);
	const setPreviewSelection = useGraphStore((s) => s.setPreviewSelection);

	useEffect(() => {
		if (!venueId) {
			setGroups([]);
			return;
		}
		let active = true;
		invoke<FixtureGroupNode[]>("get_grouped_hierarchy", { venueId })
			.then((nodes) => {
				if (!active) return;
				const names = nodes
					.map((n) => n.groupName)
					.filter((n): n is string => !!n);
				setGroups([...new Set(names)].sort());
			})
			.catch(() => active && setGroups([]));
		return () => {
			active = false;
		};
	}, [venueId]);

	// Active groups = the |-separated tokens of the current expression that match
	// a known group name (complex agent expressions still apply; they just won't
	// light up extra toggles).
	const activeTokens = previewSelection
		? new Set(
				previewSelection
					.split("|")
					.map((s) => s.trim())
					.filter(Boolean),
			)
		: new Set<string>();

	const toggle = (name: string) => {
		const next = new Set(activeTokens);
		if (next.has(name)) next.delete(name);
		else next.add(name);
		const expr = [...next].join(" | ");
		setPreviewSelection(expr.length > 0 ? expr : null);
	};

	if (groups.length === 0) return null;

	return (
		<div className="flex items-center gap-1 px-3 py-1.5 bg-card border-t border-trim overflow-x-auto">
			<span className="text-[9px] font-bold uppercase tracking-wider text-muted-foreground shrink-0 mr-1">
				Preview on
			</span>
			<Toggle
				pressed={activeTokens.size === 0}
				onClick={() => setPreviewSelection(null)}
			>
				All
			</Toggle>
			{groups.map((name) => (
				<Toggle
					key={name}
					pressed={activeTokens.has(name)}
					onClick={() => toggle(name)}
				>
					{name}
				</Toggle>
			))}
		</div>
	);
}

type PatternEditorProps = {
	patternId: string;
	nodeTypes: NodeTypeDef[];
};

/// Visualizer wrapper. The render time is read imperatively inside the
/// visualizer's frame loop — a hook subscription here would re-render the
/// entire StageVisualizer tree on every host-audio snapshot, and its DOM
/// commits force document layout passes that stall pointer interactions.
function VisualizerStage({
	instanceStartTime,
}: {
	instanceStartTime: number | null;
}) {
	const getRenderAudioTime = useCallback(() => {
		// Extrapolated between snapshots (which arrive at only a few Hz) so the
		// 3D render time advances smoothly at display rate.
		const t = getExtrapolatedHostTime();
		return instanceStartTime !== null && Number.isFinite(t)
			? instanceStartTime + t
			: t;
	}, [instanceStartTime]);
	return (
		<StageVisualizer
			enableEditing={false}
			getRenderAudioTime={getRenderAudioTime}
		/>
	);
}

export function PatternEditor({ patternId, nodeTypes }: PatternEditorProps) {
	const [loading, setLoading] = useState(true);
	const [error, setError] = useState<string | null>(null);
	const [graphError, setGraphError] = useState<string | null>(null);
	const [loadedGraph, setLoadedGraph] = useState<Graph | null>(null);
	const [implementationId, setImplementationId] = useState<string | null>(null);
	const [editorReady, setEditorReady] = useState(false);
	const [isSaving, setIsSaving] = useState(false);
	const [isBuildingGraph, setIsBuildingGraph] = useState(false);
	const [instances, setInstances] = useState<PatternAnnotationInstance[]>([]);
	const [instancesLoading, setInstancesLoading] = useState(false);
	const [instancesError, setInstancesError] = useState<string | null>(null);
	const [selectedInstanceId, setSelectedInstanceId] = useState<string | null>(
		null,
	);
	const [pendingInstanceId, setPendingInstanceId] = useState<string | null>(
		null,
	);
	const [pattern, setPattern] = useState<PatternSummary | null>(null);
	const [patternLoading, setPatternLoading] = useState(true);
	const [patternArgs, setPatternArgs] = useState<PatternArgDef[]>([]);
	const [pendingDeleteArgId, setPendingDeleteArgId] = useState<string | null>(
		null,
	);
	const [argDialogOpen, setArgDialogOpen] = useState(false);
	const [editingArgId, setEditingArgId] = useState<string | null>(null);
	const [newArgName, setNewArgName] = useState("");
	const normalizedArgName = toSnakeCase(newArgName);
	const [newArgColor, setNewArgColor] = useState("#ff0000");
	const [newArgScalar, setNewArgScalar] = useState(1.0);
	const [newArgExpression, setNewArgExpression] = useState("all");
	const [newArgPalette, setNewArgPalette] = useState<PaletteValue>({
		colors: ["#ff0080", "#00ffc8", "#ffbe28"],
	});
	const [newArgGradient, setNewArgGradient] = useState<GradientValue>({
		stops: [
			{ color: "#000000", t: 0 },
			{ color: "#ffffff", t: 1 },
		],
	});
	const [newArgType, setNewArgType] = useState<PatternArgType>("Color");
	const [contextSheetOpen, setContextSheetOpen] = useState(false);
	const [rightPanelTab, setRightPanelTab] = useState<"info" | "agent">("info");
	const [isForking, setIsForking] = useState(false);
	// `hostCurrentTime` is deliberately NOT subscribed at this level — it ticks
	// at audio rate (30-60Hz) and the whole PatternEditor tree (React Flow,
	// dialogs, context sheets) would re-render every tick. The visualizer
	// reads it directly via <VisualizerStage>.
	const currentVenue = useAppViewStore((s) => s.currentVenue);
	const currentUserId = useAuthStore((s) => s.user?.id ?? null);
	const isOwner = pattern?.uid === currentUserId;
	const verifyPattern = usePatternsStore((s) => s.verifyPattern);
	const forkPatternAction = usePatternsStore((s) => s.forkPattern);
	const selectionPreviewSeed = useGraphStore((s) => s.selectionPreviewSeed);
	const setSelectionPreviewSeed = useGraphStore(
		(s) => s.setSelectionPreviewSeed,
	);
	const previewSelection = useGraphStore((s) => s.previewSelection);
	const selectedInstance = useMemo(
		() => instances.find((inst) => inst.id === selectedInstanceId) ?? null,
		[instances, selectedInstanceId],
	);
	useEffect(() => {
		if (selectedInstance) {
			setGraphError(null);
		}
	}, [selectedInstance]);
	useEffect(() => {
		setSelectionPreviewSeed(generateSelectionSeed());
	}, [setSelectionPreviewSeed]);

	useEffect(() => {
		if (isBuildingGraph) {
			// Live param edits re-run the graph continuously; only surface the
			// toast when a build is actually slow, not on every drag tick.
			const timer = setTimeout(() => {
				toast.loading("Building graph…", { id: "building-graph" });
			}, 400);
			return () => {
				clearTimeout(timer);
				toast.dismiss("building-graph");
			};
		}
		toast.dismiss("building-graph");
	}, [isBuildingGraph]);

	const navigate = useNavigate();
	const location = useLocation();
	const pendingInstanceIdRef = useRef<string | null>(
		(location.state as { instanceId?: string } | null)?.instanceId ?? null,
	);
	const editorRef = useRef<EditorController | null>(null);
	const pendingRunId = useRef(0);
	const graphRunInFlightRef = useRef(false);
	const queuedGraphRef = useRef<{
		graph: Graph;
		includeMelSpecs: boolean;
	} | null>(null);
	const executeGraphRef = useRef<
		| ((graph: Graph, opts?: { includeMelSpecs?: boolean }) => Promise<void>)
		| null
	>(null);
	const goBack = useCallback(() => navigate(-1), [navigate]);
	const hasHydratedGraphRef = useRef(false);
	const savedGraphFingerprintRef = useRef<string | null>(null);
	const savedGraphRevisionRef = useRef<string | null>(null);
	/** Orders authoritative graph reads/projections so a delayed load or save
	 * response cannot overwrite a newer agent finalization or recovery. */
	const graphAuthorityGateRef = useRef(new LatestRequestGate());
	const forkRequestGateRef = useRef(new IdempotentRequestGate());
	const lastPatternArgsHashRef = useRef<string | null>(null);
	// In-memory working graph the agent's tools mutate within a turn — re-seeded
	// from the live canvas at each turn start so edits don't race React state.
	const agentWorkingRef = useRef<Graph | null>(null);
	const refreshSelectionSeed = useCallback(() => {
		setSelectionPreviewSeed(generateSelectionSeed());
	}, [setSelectionPreviewSeed]);
	const handleForkPattern = useCallback(async () => {
		if (!implementationId) return;
		const fingerprint = JSON.stringify([
			currentUserId ?? "signed-out",
			patternId,
			implementationId,
		]);
		const request = forkRequestGateRef.current.begin(fingerprint);
		if (!request) return;
		setIsForking(true);

		let forked: PatternSummary;
		try {
			forked = await forkPatternAction({
				sourcePatternId: patternId,
				sourceImplementationId: implementationId,
				requestId: request.requestId,
			});
		} catch (error) {
			if (!forkRequestGateRef.current.fail(request)) return;
			setIsForking(false);
			console.error("Failed to fork pattern", error);
			toast.error("Failed to fork pattern", {
				description: error instanceof Error ? error.message : String(error),
			});
			return;
		}

		if (!forkRequestGateRef.current.succeed(request)) return;
		setIsForking(false);
		navigate(`/pattern/${forked.id}`, {
			state: { name: forked.name },
		});
	}, [currentUserId, forkPatternAction, implementationId, navigate, patternId]);
	useEffect(() => {
		forkRequestGateRef.current.reset();
		setIsForking(false);
		return () => {
			forkRequestGateRef.current.reset();
		};
	}, [currentUserId, implementationId, patternId]);
	const patternArgsNodeDef = useMemo<NodeTypeDef | null>(
		() => patternArgsNodeDefFor(patternArgs),
		[patternArgs],
	);
	const getNodeDefinitions = useCallback(() => {
		const base = nodeTypes;
		return patternArgsNodeDef ? [...base, patternArgsNodeDef] : base;
	}, [nodeTypes, patternArgsNodeDef]);

	const loadInstances = useCallback(async () => {
		setInstancesLoading(true);
		setInstancesError(null);
		try {
			const tracks = await invoke<TrackSummary[]>("list_tracks");
			const collected: PatternAnnotationInstance[] = [];

			for (const track of tracks) {
				const annotations: TrackScore[] = [];
				// scoreId -> venueId, so each instance carries its score's venue.
				const scoreVenues = new Map<string, string | null>();
				try {
					// Always pass an empty venueId to list the track's scores across
					// ALL venues. A pattern can be used in any venue, and the preview
					// should work regardless of which venue (if any) is currently
					// active — filtering by currentVenue would hide instances that
					// only exist in other venues, leaving nothing to preview.
					const scores = await invoke<{ id: string; venueId: string | null }[]>(
						"list_scores_across_venues",
						{ trackId: track.id },
					);
					for (const score of scores) {
						scoreVenues.set(score.id, score.venueId ?? null);
						const trackScores = await invoke<TrackScore[]>(
							"list_track_scores",
							{ scoreId: score.id },
						);
						annotations.push(...trackScores);
					}
				} catch (err) {
					console.error(
						`[PatternEditor] Failed to load annotations for track ${track.id}`,
						err,
					);
				}
				const matching = annotations.filter(
					(ann) => ann.patternId === patternId,
				);
				if (matching.length === 0) continue;

				const [beatGrid, waveform] = await Promise.all([
					invoke<BeatGrid | null>("get_track_beats", {
						trackId: track.id,
					}).catch((err) => {
						console.error(
							`[PatternEditor] Failed to load beat grid for track ${track.id}`,
							err,
						);
						return null;
					}),
					invoke<TrackWaveform | null>("get_track_waveform", {
						trackId: track.id,
					}).catch((err) => {
						console.error(
							`[PatternEditor] Failed to load waveform for track ${track.id}`,
							err,
						);
						return null;
					}),
				]);

				for (const ann of matching) {
					const windowedGrid = sliceBeatGrid(
						beatGrid,
						ann.startTime,
						ann.endTime,
					);
					collected.push({
						...ann,
						track,
						beatGrid: windowedGrid,
						waveform,
						venueId: scoreVenues.get(ann.scoreId) ?? null,
					});
				}
			}

			// Randomly sample up to 10 instances, ensuring the pending instance is included
			const priorityId = pendingInstanceIdRef.current;
			let sampled: PatternAnnotationInstance[];
			if (collected.length <= 10) {
				sampled = collected;
			} else {
				const priority =
					priorityId !== null
						? collected.find((inst) => inst.id === priorityId)
						: null;
				const rest = priority
					? collected.filter((inst) => inst.id !== priorityId)
					: collected;
				const randomized = rest
					.map((inst) => ({ inst, sort: Math.random() }))
					.sort((a, b) => a.sort - b.sort)
					.slice(0, priority ? 9 : 10)
					.map((x) => x.inst);
				sampled = priority ? [priority, ...randomized] : randomized;
			}

			setInstances(sampled);
			// Select the pending instance directly if available, otherwise default to first
			const priorityMatch =
				priorityId !== null
					? sampled.find((inst) => inst.id === priorityId)
					: null;
			if (priorityMatch) {
				setSelectedInstanceId(priorityMatch.id);
			} else if (sampled.length > 0) {
				setSelectedInstanceId((prev) => prev ?? sampled[0].id);
			}
		} catch (err) {
			console.error("[PatternEditor] Failed to load context instances", err);
			setInstances([]);
			setInstancesError(
				err instanceof Error ? err.message : String(err ?? "Failed to load"),
			);
		} finally {
			setInstancesLoading(false);
		}
	}, [patternId]);

	useEffect(() => {
		// Ensure fixtures are loaded for the visualizer
		useFixtureStore.getState().initialize();
	}, []);

	// Keep the visualizer's fixtures in sync with the selected instance's venue
	// (the editor runs outside a venue route, so the fixture store's venue is
	// whatever screen the user came from — possibly another venue entirely).
	const instanceVenueId = selectedInstance?.venueId ?? null;
	useEffect(() => {
		if (
			instanceVenueId !== null &&
			useFixtureStore.getState().venueId !== instanceVenueId
		) {
			useFixtureStore.getState().initialize(instanceVenueId);
		}
	}, [instanceVenueId]);

	useEffect(() => {
		return () => {
			useFixtureStore.getState().clearPreviewFixtureIds();
		};
	}, []);

	// Load pattern metadata
	useEffect(() => {
		let active = true;
		setPatternLoading(true);

		invoke<PatternSummary>("get_pattern", { id: patternId })
			.then((p) => {
				if (active) {
					setPattern(p);
				}
			})
			.catch((err) => {
				console.error("[PatternEditor] Failed to load pattern", err);
			})
			.finally(() => {
				if (active) {
					setPatternLoading(false);
				}
			});

		return () => {
			active = false;
		};
	}, [patternId]);

	useEffect(() => {
		loadInstances();
	}, [loadInstances]);

	useEffect(() => {
		const id =
			(location.state as { instanceId?: string } | null)?.instanceId ?? null;
		setPendingInstanceId(id);
		pendingInstanceIdRef.current = id;
	}, [patternId, location.state]);

	useEffect(() => {
		if (pendingInstanceId === null) return;
		const matched = instances.find((inst) => inst.id === pendingInstanceId);
		if (matched) {
			setSelectedInstanceId(matched.id);
			setPendingInstanceId(null);
			return;
		}
		if (!instancesLoading) {
			setPendingInstanceId(null);
		}
	}, [pendingInstanceId, instances, instancesLoading]);

	useEffect(() => {
		if (
			selectedInstanceId !== null &&
			instances.some((inst) => inst.id === selectedInstanceId)
		) {
			return;
		}
		if (instances.length > 0) {
			setSelectedInstanceId(instances[0].id);
		}
	}, [instances, selectedInstanceId]);

	// Subscribe to host audio state broadcasts
	useEffect(() => {
		let unsub: (() => void) | null = null;
		let cancelled = false;
		const store = useHostAudioStore;
		const handleSnapshot = (snapshot: HostAudioSnapshot) => {
			store.getState().handleSnapshot(snapshot);
		};
		const reset = () => store.getState().reset();

		listen<HostAudioSnapshot>("host-audio://state", (event) => {
			handleSnapshot(event.payload);
		})
			.then((unlisten) => {
				if (cancelled) {
					unlisten();
				} else {
					unsub = unlisten;
				}
			})
			.catch((err) => {
				console.error(
					"[PatternEditor] Failed to subscribe to host audio state",
					err,
				);
			});

		invoke<HostAudioSnapshot>("host_snapshot")
			.then((snapshot) => {
				if (!cancelled) {
					handleSnapshot(snapshot);
				}
			})
			.catch((err) => {
				console.error(
					"[PatternEditor] Failed to fetch host audio snapshot",
					err,
				);
			});

		return () => {
			cancelled = true;
			if (unsub) {
				unsub();
			}
			reset();
			resetViewDataStore();
		};
	}, []);

	const updateViewResults = useCallback(
		(
			views: Record<string, Signal>,
			melSpecs: Record<string, MelSpec>,
			colorViews: Record<string, string>,
		) => {
			if (!editorRef.current) return;
			editorRef.current.updateViewData(views, melSpecs, colorViews);
		},
		[],
	);

	const executeGraph = useCallback(
		async (graph: Graph, opts?: { includeMelSpecs?: boolean }) => {
			// Mel specs depend only on audio wiring + span, so param-only edits
			// (slider drags) skip their FFT recompute and heavy payload.
			const includeMelSpecs = opts?.includeMelSpecs ?? true;
			if (!selectedInstance) {
				// Don't error when no context is selected; just skip execution.
				setGraphError(null);
				setIsBuildingGraph(false);
				return;
			}

			if (graph.nodes.length === 0) {
				setGraphError(null);
				await updateViewResults({}, {}, {});
				setIsBuildingGraph(false);
				return;
			}

			// During param drags edits stream in faster than the backend
			// round-trip; keep one invoke in flight and stash only the latest
			// graph to run when it settles. OR the mel-spec flag so a queued
			// structural change is never downgraded by a later param edit.
			if (graphRunInFlightRef.current) {
				queuedGraphRef.current = {
					graph,
					includeMelSpecs:
						includeMelSpecs ||
						(queuedGraphRef.current?.includeMelSpecs ?? false),
				};
				return;
			}
			graphRunInFlightRef.current = true;

			const runId = ++pendingRunId.current;
			setIsBuildingGraph(true);

			try {
				// Context is now passed separately from the graph
				// The graph stays pure (no track-specific params injected)
				const instanceArgs =
					(selectedInstance.args as Record<string, unknown> | undefined) ?? {};
				const mergedArgValues = buildRunArgValues(
					patternArgs ?? [],
					instanceArgs,
					previewSelection,
				);
				const context: GraphContextWithSeed = {
					trackId: selectedInstance.track.id,
					// The instance's score venue — the global currentVenue is cleared
					// outside /venue/* routes, and an empty venue resolves the
					// selection to zero fixtures (black output).
					venueId: selectedInstance.venueId ?? currentVenue?.id ?? "",
					startTime: selectedInstance.startTime,
					endTime: selectedInstance.endTime,
					beatGrid: selectedInstance.beatGrid,
					argValues: mergedArgValues,
					instanceSeed: selectionPreviewSeed ?? undefined,
				};

				const result = await invoke<RunResult>("run_graph", {
					graph,
					context,
					includeMelSpecs,
				});
				if (runId !== pendingRunId.current) return;

				setGraphError(null);
				await updateViewResults(
					result.views ?? {},
					result.melSpecs ?? {},
					result.colorViews ?? {},
				);
			} catch (err) {
				if (runId !== pendingRunId.current) return;
				console.error("Failed to execute graph", err);
				setGraphError(err instanceof Error ? err.message : String(err));
			} finally {
				graphRunInFlightRef.current = false;
				if (runId === pendingRunId.current) {
					setIsBuildingGraph(false);
				}
				const queued = queuedGraphRef.current;
				queuedGraphRef.current = null;
				if (queued) {
					void executeGraphRef.current?.(queued.graph, {
						includeMelSpecs: queued.includeMelSpecs,
					});
				}
			}
		},
		[
			updateViewResults,
			selectedInstance,
			patternArgs,
			selectionPreviewSeed,
			previewSelection,
			currentVenue,
		],
	);
	useEffect(() => {
		executeGraphRef.current = executeGraph;
	}, [executeGraph]);

	// Load host audio segment when instance changes
	useEffect(() => {
		if (!selectedInstance) return;

		// Load the audio segment into host audio state for playback
		useHostAudioStore
			.getState()
			.loadSegment(
				selectedInstance.track.id,
				selectedInstance.startTime,
				selectedInstance.endTime,
				selectedInstance.beatGrid,
			)
			.catch((err) => {
				console.error("[PatternEditor] Failed to load audio segment", err);
			});
	}, [selectedInstance]);

	useEffect(() => {
		if (!editorReady || !selectedInstance) return;

		// Update visual context on nodes
		if (editorRef.current) {
			const trackName =
				selectedInstance.track.title ??
				selectedInstance.track.filePath ??
				"Track";
			const timeLabel = `${formatTime(
				selectedInstance.startTime,
			)} – ${formatTime(selectedInstance.endTime)}`;
			editorRef.current.updateNodeContext({
				trackName,
				timeLabel,
			});
		}

		const graph = editorRef.current?.serialize();
		if (graph) {
			executeGraph(graph);
		}
	}, [selectedInstance, executeGraph, editorReady]);

	// Load pattern graph on mount - wait for nodeTypes to be available
	useEffect(() => {
		const requestTicket = graphAuthorityGateRef.current.issue();
		hasHydratedGraphRef.current = false;
		savedGraphFingerprintRef.current = null;
		savedGraphRevisionRef.current = null;
		setImplementationId(null);
		let active = true;
		setLoading(true);
		setError(null);

		invoke<{ implementationId: string; revision: string; graph: Graph }>(
			"get_pattern_graph_document",
			{
				id: patternId,
				implementationId: null,
			},
		)
			.then((document) => {
				if (!active || !graphAuthorityGateRef.current.owns(requestTicket))
					return;
				try {
					setImplementationId(document.implementationId);
					savedGraphRevisionRef.current = document.revision;
					const graph = document.graph;
					setPatternArgs((prev) => {
						const next = graph.args ?? [];
						const prevHash = JSON.stringify(prev ?? []);
						const nextHash = JSON.stringify(next);
						if (prevHash === nextHash) {
							return prev;
						}
						return next;
					});
					const withArgs = withPatternArgsNode(graph, graph.args ?? []);
					// Store graph to load when editor ref is ready
					setLoadedGraph(withArgs);
				} catch (err) {
					console.error("[PatternEditor] Failed to parse graph JSON", err);
					setError(
						err instanceof Error ? err.message : "Failed to parse graph JSON",
					);
				}
			})
			.catch((err) => {
				if (!active || !graphAuthorityGateRef.current.owns(requestTicket))
					return;
				console.error("[PatternEditor] Failed to load pattern graph", err);
				setError(err instanceof Error ? err.message : String(err));
			})
			.finally(() => {
				if (!active || !graphAuthorityGateRef.current.owns(requestTicket))
					return;
				setLoading(false);
			});

		return () => {
			active = false;
		};
	}, [patternId]);

	// Load graph into editor when both graph and editor are ready
	useEffect(() => {
		if (!loadedGraph || !editorReady || !editorRef.current) {
			return;
		}

		editorRef.current.loadGraph(loadedGraph, getNodeDefinitions);
		hasHydratedGraphRef.current = true;
		savedGraphFingerprintRef.current = graphFingerprint(loadedGraph);
		// Set initial args hash to prevent false positive change detection
		lastPatternArgsHashRef.current = JSON.stringify(loadedGraph.args ?? []);

		// Execute the graph after loading
		if (selectedInstance) {
			setTimeout(async () => {
				await executeGraph(loadedGraph);
			}, 100);
		}

		// Clear loaded graph after loading to avoid reloading
		setLoadedGraph(null);
	}, [loadedGraph, editorReady, nodeTypes, executeGraph, getNodeDefinitions]);

	const serializeGraph = useCallback((): Graph | null => {
		if (!editorRef.current) return null;
		const graph = editorRef.current.serialize();
		const withArgs = withPatternArgsNode(
			{ ...graph, args: patternArgs },
			patternArgs,
		);
		return withArgs;
	}, [patternArgs]);

	// Block navigation when there are unsaved changes
	const blocker = useBlocker(() => {
		if (!isOwner) return false;
		const current = serializeGraph();
		if (!current) return false;
		return graphFingerprint(current) !== savedGraphFingerprintRef.current;
	});

	useEffect(() => {
		if (!editorReady || !editorRef.current) return;
		// Don't reload graph if we haven't hydrated it yet (initial load)
		if (!hasHydratedGraphRef.current) return;
		const argsHash = JSON.stringify(patternArgs ?? []);
		if (patternArgs.length === 0) {
			// Avoid overwriting the graph when there are no pattern args defined
			// (initial load sets patternArgs to [] which would serialize only required nodes)
			return;
		}
		if (argsHash === lastPatternArgsHashRef.current) {
			return;
		}
		lastPatternArgsHashRef.current = argsHash;
		const graph = serializeGraph();
		if (!graph) return;
		editorRef.current.loadGraph(graph, getNodeDefinitions);
		if (selectedInstance) {
			void executeGraph(graph);
		}
	}, [
		patternArgs,
		editorReady,
		getNodeDefinitions,
		serializeGraph,
		selectedInstance,
		patternId,
	]);

	const checkpointGraph = useCallback(
		async (graph: Graph): Promise<void> => {
			const fingerprint = graphFingerprint(graph);
			if (fingerprint === savedGraphFingerprintRef.current) return;
			if (!implementationId) {
				throw new Error("Graph implementation is not loaded yet");
			}
			const baseRevision = savedGraphRevisionRef.current;
			if (!baseRevision) {
				throw new Error("Graph revision is not loaded yet");
			}

			const requestTicket = graphAuthorityGateRef.current.issue();
			await checkpointGraphDocument({
				patternId,
				implementationId,
				baseRevision,
				graph,
				save: (input) =>
					invoke<GraphEditResult>("save_pattern_graph_document", input),
				accept: (authoritative) => {
					if (!graphAuthorityGateRef.current.owns(requestTicket)) {
						throw new Error("Graph checkpoint was superseded by newer state.");
					}
					const exact = structuredClone(authoritative.graph);
					const args = exact.args ?? [];
					lastPatternArgsHashRef.current = JSON.stringify(args);
					setPatternArgs(args);
					agentWorkingRef.current = exact;
					setLoadedGraph(withPatternArgsNode(exact, args));
					savedGraphRevisionRef.current = authoritative.revision;
					savedGraphFingerprintRef.current = graphFingerprint(exact);
				},
			});
			if (!graphAuthorityGateRef.current.owns(requestTicket)) {
				throw new Error("Graph checkpoint was superseded by newer state.");
			}
		},
		[implementationId, patternId],
	);

	// Save graph through the relational document authority (manual save only).
	const saveGraph = useCallback(async () => {
		const graph = serializeGraph();
		if (!graph) return;

		setIsSaving(true);
		try {
			await checkpointGraph(graph);
			setError(null);
		} catch (err) {
			console.error("[PatternEditor] Failed to save pattern graph", err);
			setError(err instanceof Error ? err.message : "Failed to save");
		} finally {
			setIsSaving(false);
		}
	}, [checkpointGraph, serializeGraph]);

	const executeCurrentGraph = useCallback(async () => {
		const graph = serializeGraph();
		if (!graph) return;
		await executeGraph(graph);
	}, [serializeGraph, executeGraph]);

	// Register the live editor bridge for the graph agent. Tools resolve this
	// lazily, so the long-lived chat session always acts on the current canvas.
	useEffect(() => {
		if (!implementationId) return;
		const liveGraph = (): Graph => {
			const g = editorRef.current?.serialize();
			return g
				? { nodes: g.nodes, edges: g.edges, args: patternArgs }
				: { nodes: [], edges: [], args: patternArgs };
		};
		const venueId = selectedInstance?.venueId ?? currentVenue?.id ?? null;
		return graphAgent.registerBridge(
			patternId,
			{
				patternId,
				implementationId,
				// Reads come from the working copy (seeded at turn start); fall back to
				// the live canvas if no turn is in progress.
				serialize: () => agentWorkingRef.current ?? liveGraph(),
				syncFromEditor: () => {
					agentWorkingRef.current = liveGraph();
				},
				apply: (graph) => {
					// Auto-tidy: the agent only sets nodes/edges, not positions, so lay
					// the graph out left→right by signal flow. Deterministic, so
					// param-only edits don't reshuffle the canvas.
					const laid = layoutGraph(graph);
					agentWorkingRef.current = laid;
					editorRef.current?.loadGraph(laid, getNodeDefinitions);
				},
				restore: (graph, revision) => {
					graphAuthorityGateRef.current.supersede();
					const exact = structuredClone(graph);
					const argsHash = JSON.stringify(exact.args);
					lastPatternArgsHashRef.current = argsHash;
					setPatternArgs(exact.args);
					agentWorkingRef.current = exact;
					if (editorReady && editorRef.current) {
						setLoadedGraph(null);
						editorRef.current.loadGraph(exact, getNodeDefinitions);
						hasHydratedGraphRef.current = true;
					} else {
						setLoadedGraph(exact);
					}
					savedGraphRevisionRef.current = revision;
					savedGraphFingerprintRef.current = graphFingerprint(exact);
					setLoading(false);
					setError(null);
				},
				checkpoint: () => checkpointGraph(liveGraph()),
				run: async (graph, opts) => {
					if (!selectedInstance) {
						throw new Error(
							"Select a track context (top-left of the preview) so the graph can run.",
						);
					}
					const instanceArgs =
						(selectedInstance.args as Record<string, unknown> | undefined) ??
						{};
					const runPreviewSelection =
						opts && "previewSelection" in opts
							? (opts.previewSelection ?? null)
							: previewSelection;
					const context: GraphContextWithSeed = {
						trackId: selectedInstance.track.id,
						venueId: selectedInstance.venueId ?? currentVenue?.id ?? "",
						startTime: selectedInstance.startTime,
						endTime: selectedInstance.endTime,
						beatGrid: selectedInstance.beatGrid,
						argValues: buildRunArgValues(
							graph.args ?? [],
							instanceArgs,
							runPreviewSelection,
						),
						instanceSeed: selectionPreviewSeed ?? undefined,
					};
					const result = await invoke<SchemaRunResult>("run_graph", {
						graph,
						context,
						includeMelSpecs: true,
						// When the agent runs, publish the evaluation to its thread's
						// Python workspace (`luma.graph.run`).
						agentThreadId: opts?.agentThreadId,
						agentExecutionId: opts?.agentExecutionId,
						driveLivePreview: opts?.driveLivePreview,
					});
					if (opts?.driveLivePreview !== false) {
						// Root runs mirror onto the canvas; detached child runs stay private.
						await updateViewResults(
							(result.views ?? {}) as Record<string, Signal>,
							(result.melSpecs ?? {}) as Record<string, MelSpec>,
							(result.colorViews ?? {}) as Record<string, string>,
						);
					}
					return result;
				},
				previewImage: async (graph, opts) => {
					if (!selectedInstance) {
						throw new Error(
							"Select a track context (top-left of the preview) so the graph can render.",
						);
					}
					const imagePreviewSelection =
						opts && "previewSelection" in opts
							? (opts.previewSelection ?? null)
							: previewSelection;
					return invoke<AnnotationPreview>("preview_graph_image", {
						graph: {
							...graph,
							args: overrideSelectionArgs(graph.args, imagePreviewSelection),
						},
						trackId: selectedInstance.track.id,
						venueId: selectedInstance.venueId ?? currentVenue?.id ?? "",
						startTime: selectedInstance.startTime,
						endTime: selectedInstance.endTime,
						beatGrid: selectedInstance.beatGrid,
					});
				},
				setArgs: (args) => {
					setPatternArgs(args);
					const g = editorRef.current?.serialize();
					if (g && editorRef.current) {
						const withNode = withPatternArgsNode({ ...g, args }, args);
						agentWorkingRef.current = {
							nodes: withNode.nodes,
							edges: withNode.edges,
							args,
						};
						editorRef.current.loadGraph(withNode, getNodeDefinitions);
						void executeGraph(withNode);
					}
				},
				setPreviewSelection: (expr) =>
					useGraphStore.getState().setPreviewSelection(expr),
				getVenueId: () => selectedInstance?.venueId ?? currentVenue?.id ?? null,
				getTrackId: () => selectedInstance?.track.id ?? null,
				getNodeDefs: getNodeDefinitions,
				getSpan: () =>
					selectedInstance
						? [selectedInstance.startTime, selectedInstance.endTime]
						: [0, 1],
				// Rendered into the system prompt, which is a cached prefix: keep
				// this stable while the agent works. Args are deliberately not
				// enumerated — they change under the agent's own edits and are
				// readable through the graph tools.
				describe: () => {
					const name = pattern?.name ?? patternId;
					const ctx = selectedInstance
						? `Preview context: "${selectedInstance.track.title ?? selectedInstance.track.id}".`
						: "No preview context selected (run/python will be unavailable until one is chosen).";
					return `Pattern: ${name}\n${ctx}`;
				},
			},
			{ principalId: currentUserId, implementationId, venueId },
		);
	}, [
		patternId,
		implementationId,
		currentUserId,
		editorReady,
		patternArgs,
		selectedInstance,
		currentVenue,
		selectionPreviewSeed,
		previewSelection,
		getNodeDefinitions,
		checkpointGraph,
		updateViewResults,
		executeGraph,
		pattern,
	]);

	const handleGraphChange = useCallback(
		async (change: { structural: boolean }) => {
			const graph = serializeGraph();
			if (!graph) return;
			await executeGraph(graph, { includeMelSpecs: change.structural });
		},
		[serializeGraph, executeGraph],
	);

	useEffect(() => {
		if (!editorReady) return;
		if (selectionPreviewSeed === null) return;
		void executeCurrentGraph();
	}, [selectionPreviewSeed, editorReady, executeCurrentGraph]);

	// Re-render the preview when the preview-only selection changes (UI toggles
	// or the agent's set_preview_selection).
	useEffect(() => {
		if (!editorReady || !selectedInstance) return;
		void executeCurrentGraph();
	}, [previewSelection, editorReady, selectedInstance, executeCurrentGraph]);

	const handleEditArg = useCallback((arg: PatternArgDef) => {
		setEditingArgId(arg.id);
		setNewArgName(arg.name);
		setNewArgType(arg.argType);
		if (arg.argType === "Color") {
			const c = arg.defaultValue as {
				r: number;
				g: number;
				b: number;
				a: number;
			};
			const toHex = (v: number) =>
				Math.round(Number(v)).toString(16).padStart(2, "0");
			const hex = `#${toHex(c.r)}${toHex(c.g)}${toHex(c.b)}${toHex(
				Math.round(c.a * 255),
			)}`;
			setNewArgColor(hex);
		} else if (arg.argType === "Scalar") {
			setNewArgScalar(arg.defaultValue as unknown as number);
		} else if (arg.argType === "Selection") {
			const sel = arg.defaultValue as { expression: string };
			setNewArgExpression(sel.expression ?? "all");
		} else if (arg.argType === "Palette") {
			setNewArgPalette(
				(arg.defaultValue as PaletteValue | undefined) ?? {
					colors: ["#ff0080"],
				},
			);
		} else if (arg.argType === "Gradient") {
			setNewArgGradient(
				(arg.defaultValue as GradientValue | undefined) ?? {
					stops: [
						{ color: "#000000", t: 0 },
						{ color: "#ffffff", t: 1 },
					],
				},
			);
		}
		setArgDialogOpen(true);
	}, []);

	const handleDeleteArg = useCallback((argId: string) => {
		setPendingDeleteArgId(argId);
	}, []);

	const confirmDeleteArg = useCallback(() => {
		if (!pendingDeleteArgId) return;
		setPatternArgs((prev) => {
			const arg = prev.find((a) => a.id === pendingDeleteArgId);
			// Prevent deleting the last Selection arg
			if (arg?.argType === "Selection") {
				const selectionCount = prev.filter(
					(a) => a.argType === "Selection",
				).length;
				if (selectionCount <= 1) return prev;
			}
			return prev.filter((a) => a.id !== pendingDeleteArgId);
		});
		setPendingDeleteArgId(null);
	}, [pendingDeleteArgId]);

	const handleRenamePattern = useCallback(
		async (name: string) => {
			try {
				const updated = await invoke<PatternSummary>("update_pattern", {
					id: patternId,
					name,
					description: pattern?.description ?? null,
				});
				setPattern(updated);
			} catch (err) {
				console.error("[PatternEditor] Failed to rename pattern", err);
			}
		},
		[patternId, pattern?.description],
	);

	const handleSetCategory = useCallback(
		async (categoryName: string | null) => {
			try {
				await invoke("set_pattern_category", {
					patternId,
					categoryName,
				});
				const updated = await invoke<PatternSummary>("get_pattern", {
					id: patternId,
				});
				setPattern(updated);
			} catch (err) {
				console.error("[PatternEditor] Failed to set category", err);
			}
		},
		[patternId],
	);

	const handleUpdateDescription = useCallback(
		async (description: string | null) => {
			try {
				const updated = await invoke<PatternSummary>("update_pattern", {
					id: patternId,
					name: pattern?.name ?? "",
					description,
				});
				setPattern(updated);
			} catch (err) {
				console.error("[PatternEditor] Failed to update description", err);
			}
		},
		[patternId, pattern?.name],
	);

	const annotationContextValue = useMemo(
		() => ({
			instances,
			selectedId: selectedInstanceId,
			selectInstance: setSelectedInstanceId,
			loading: instancesLoading,
		}),
		[instances, selectedInstanceId, instancesLoading],
	);

	if (loading) {
		return (
			<div className="flex h-full items-center justify-center">
				<p className="text-muted-foreground">Loading pattern...</p>
			</div>
		);
	}

	if (error) {
		return (
			<div className="flex h-full flex-col items-center justify-center gap-4">
				<p className="text-destructive">{error}</p>
				<button
					type="button"
					onClick={goBack}
					className="text-sm text-muted-foreground hover:text-foreground"
				>
					Back to patterns
				</button>
			</div>
		);
	}

	return (
		<>
			<PatternAnnotationProvider value={annotationContextValue}>
				<div className="flex h-full flex-col">
					<div className="relative flex flex-1 min-h-0">
						{instances.length > 0 && (
							<ContextSheet
								instances={instances}
								loading={instancesLoading}
								error={instancesError}
								selectedId={selectedInstanceId}
								open={contextSheetOpen}
								onSelect={(id) => {
									setSelectedInstanceId(id);
									setContextSheetOpen(false);
								}}
								onReload={loadInstances}
								onClose={() => setContextSheetOpen(false)}
							/>
						)}
						{contextSheetOpen && (
							<button
								type="button"
								aria-label="Close context panel"
								onClick={() => setContextSheetOpen(false)}
								className="absolute inset-0 z-30 bg-black/40 transition-opacity"
							/>
						)}
						{/* Node graph fills the left; visualizer + info stack on the right */}
						<div className="flex-1 bg-trim relative min-h-0 overflow-hidden">
							{graphError && (
								<div className="pointer-events-none absolute inset-x-0 top-0 z-20 flex items-center justify-center rounded-b-md bg-red-500/20 px-4 py-2 text-sm font-semibold text-red-700 shadow-sm backdrop-blur-sm">
									{graphError}
								</div>
							)}
							<ReactFlowEditorWrapper
								onChange={handleGraphChange}
								getNodeDefinitions={getNodeDefinitions}
								controllerRef={editorRef}
								readOnly={!isOwner}
								onReady={() => {
									setEditorReady(true);
								}}
							/>
							{/* Floating Toolbar */}
							<div className="absolute top-4 right-4 z-30 flex items-center gap-2">
								<Button
									onClick={refreshSelectionSeed}
									title="Refresh selection seed"
									aria-label="Refresh selection seed"
								>
									<RefreshCw />
								</Button>
								{isOwner ? (
									<Button onClick={saveGraph} disabled={isSaving}>
										<Save />
										{isSaving ? "Saving..." : "Save"}
									</Button>
								) : (
									<Button
										disabled={!implementationId || isForking}
										onClick={handleForkPattern}
									>
										<GitFork />
										{isForking ? "Forking..." : "Fork to edit"}
									</Button>
								)}
							</div>
						</div>

						{/* Right column: visualizer above the tabbed info / agent panel */}
						<div className="w-[40%] min-w-96 flex flex-col min-h-0 border-l-2 border-gutter">
							<div className="h-[45%] relative min-h-0 overflow-hidden bg-card">
								<VisualizerStage
									instanceStartTime={selectedInstance?.startTime ?? null}
								/>
								{instances.length > 0 && (
									<Button
										onClick={() => setContextSheetOpen((o) => !o)}
										className="absolute top-2 left-2 z-10 max-w-[calc(100%-1rem)]"
									>
										<Layers />
										<span className="truncate">
											{selectedInstance
												? (selectedInstance.track.title ??
													`Track ${selectedInstance.track.id}`)
												: "Select context"}
										</span>
									</Button>
								)}
								{!selectedInstance && (
									<div className="absolute inset-0 bg-black/60 flex items-center justify-center">
										<p className="text-sm text-white/70 font-medium">
											Add this pattern to a track to preview it
										</p>
									</div>
								)}
							</div>
							{selectedInstance && <TransportBar />}
							{selectedInstance && (
								<PreviewSelectionBar
									venueId={selectedInstance.venueId ?? currentVenue?.id ?? null}
								/>
							)}
							<div className="flex-1 min-h-0 flex flex-col border-t-2 border-gutter">
								<div className="flex items-center gap-1 p-1.5 bg-trim shrink-0">
									<Toggle
										pressed={rightPanelTab === "info"}
										onClick={() => setRightPanelTab("info")}
									>
										Info
									</Toggle>
									<Toggle
										pressed={rightPanelTab === "agent"}
										onClick={() => setRightPanelTab("agent")}
									>
										Agent
									</Toggle>
								</div>
								<div className="flex-1 min-h-0 overflow-hidden">
									{rightPanelTab === "info" ? (
										<PatternInfoPanel
											pattern={pattern}
											loading={patternLoading}
											args={patternArgs}
											readOnly={!isOwner}
											onAddArg={() => setArgDialogOpen(true)}
											onEditArg={handleEditArg}
											onDeleteArg={handleDeleteArg}
											onRename={handleRenamePattern}
											onUpdateDescription={handleUpdateDescription}
											onSetCategory={handleSetCategory}
											onPublish={
												isOwner
													? (publish) =>
															verifyPattern(patternId, publish).then(() =>
																invoke<PatternSummary>("get_pattern", {
																	id: patternId,
																}).then(setPattern),
															)
													: undefined
											}
										/>
									) : (
										<GraphAgentPanel
											patternId={patternId}
											implementationId={implementationId}
											venueId={
												selectedInstance?.venueId ?? currentVenue?.id ?? null
											}
											ready={editorReady && implementationId !== null}
										/>
									)}
								</div>
							</div>
						</div>
					</div>
				</div>
			</PatternAnnotationProvider>

			<Dialog
				open={argDialogOpen}
				onOpenChange={(open) => {
					setArgDialogOpen(open);
					if (!open) {
						setEditingArgId(null);
						setNewArgName("");
						setNewArgColor("#ff0000");
						setNewArgScalar(1.0);
						setNewArgExpression("all");
						setNewArgPalette({ colors: ["#ff0080", "#00ffc8", "#ffbe28"] });
						setNewArgGradient({
							stops: [
								{ color: "#000000", t: 0 },
								{ color: "#ffffff", t: 1 },
							],
						});
						setNewArgType("Color");
					}
				}}
			>
				<DialogContent className="bg-background">
					<DialogHeader>
						<DialogTitle>
							{editingArgId ? "Edit Pattern Arg" : "Add Pattern Arg"}
						</DialogTitle>
					</DialogHeader>
					<div className="space-y-4">
						<div className="space-y-2">
							<label
								htmlFor="pattern-arg-name"
								className="text-xs text-muted-foreground"
							>
								Name
							</label>
							<Input
								id="pattern-arg-name"
								autoCapitalize="off"
								value={newArgName}
								onChange={(e) => setNewArgName(e.target.value)}
								placeholder="my_arg_name"
							/>
							{newArgName && newArgName !== normalizedArgName && (
								<p className="text-[10px] text-muted-foreground">
									{normalizedArgName ? (
										<>
											Will be saved as:{" "}
											<code className="bg-muted px-1 rounded">
												{normalizedArgName}
											</code>
										</>
									) : (
										<span className="text-destructive">
											Name must contain at least one letter or number
										</span>
									)}
								</p>
							)}
						</div>
						<div className="space-y-2">
							<label
								htmlFor="pattern-arg-type"
								className="text-xs text-muted-foreground"
							>
								Type
							</label>
							<Select
								value={newArgType}
								onValueChange={(v) => setNewArgType(v as PatternArgType)}
								disabled={!!editingArgId}
							>
								<SelectTrigger id="pattern-arg-type">
									<SelectValue />
								</SelectTrigger>
								<SelectContent>
									<SelectItem value="Color">Color</SelectItem>
									<SelectItem value="Scalar">Scalar</SelectItem>
									<SelectItem value="Selection">Selection</SelectItem>
									<SelectItem value="Palette">Palette</SelectItem>
								</SelectContent>
							</Select>
						</div>
						{newArgType === "Color" && (
							<div className="space-y-2">
								<label
									htmlFor="pattern-arg-color"
									className="text-xs text-muted-foreground"
								>
									Default Color
								</label>
								<Popover>
									<PopoverTrigger asChild>
										<button
											id="pattern-arg-color"
											type="button"
											className="w-full flex items-center justify-between bg-muted rounded px-2 py-2"
										>
											<span
												className="w-6 h-6 rounded border"
												style={{ backgroundColor: newArgColor }}
											/>
											<span className="font-mono text-xs">{newArgColor}</span>
										</button>
									</PopoverTrigger>
									<PopoverContent className="w-auto bg-neutral-900 border border-neutral-800 p-3">
										<ColorPicker
											defaultValue={newArgColor}
											onChange={(rgba) => {
												if (Array.isArray(rgba) && rgba.length >= 3) {
													const toHex = (v: number) =>
														Math.round(Number(v)).toString(16).padStart(2, "0");
													const a =
														rgba.length >= 4
															? Math.round(Number(rgba[3]) * 255)
															: 255;
													const rgb = `#${toHex(rgba[0])}${toHex(rgba[1])}${toHex(rgba[2])}`;
													setNewArgColor(a === 255 ? rgb : `${rgb}${toHex(a)}`);
												}
											}}
										>
											<div className="flex flex-col gap-2">
												<ColorPickerSelection className="h-28 w-48 rounded" />
												<ColorPickerHue className="flex-1" />
												<ColorPickerCopyPaste />
												<ColorPickerAlpha />
											</div>
										</ColorPicker>
									</PopoverContent>
								</Popover>
							</div>
						)}
						{newArgType === "Scalar" && (
							<div className="space-y-2">
								<label
									htmlFor="pattern-arg-scalar"
									className="text-xs text-muted-foreground"
								>
									Default Value
								</label>
								<Input
									id="pattern-arg-scalar"
									type="number"
									step="0.1"
									value={newArgScalar}
									onChange={(e) => setNewArgScalar(Number(e.target.value))}
								/>
							</div>
						)}
						{newArgType === "Palette" && (
							<div className="space-y-2">
								<span className="text-xs text-muted-foreground">
									Default Palette
								</span>
								<PaletteSwatches
									value={newArgPalette}
									onChange={setNewArgPalette}
								/>
							</div>
						)}
						{newArgType === "Gradient" && (
							<div className="space-y-2">
								<span className="text-xs text-muted-foreground">
									Default Gradient
								</span>
								<GradientStops
									value={newArgGradient}
									onChange={setNewArgGradient}
								/>
							</div>
						)}
						{newArgType === "Selection" && (
							<div className="space-y-4">
								<div className="space-y-2">
									<span className="text-xs text-muted-foreground">
										Default Expression
									</span>
									<GroupExpressionEditor
										value={newArgExpression}
										onChange={setNewArgExpression}
										venueId={currentVenue?.id ?? null}
									/>
								</div>
							</div>
						)}
					</div>
					<DialogFooter>
						<button
							type="button"
							onClick={() => setArgDialogOpen(false)}
							className="px-3 py-2 text-sm text-muted-foreground"
						>
							Cancel
						</button>
						<button
							type="button"
							disabled={!normalizedArgName}
							onClick={() => {
								let id = editingArgId;
								if (!id) {
									const slug = normalizedArgName || "arg";
									id = slug;
									let counter = 1;
									while (patternArgs.some((a) => a.id === id)) {
										id = `${slug}_${counter++}`;
									}
								}

								let defaultValue: Record<string, unknown>;
								if (newArgType === "Color") {
									const hex = newArgColor.startsWith("#")
										? newArgColor
										: `#${newArgColor}`;
									const safe = hex.replace("#", "");
									const r = parseInt(safe.slice(0, 2), 16) || 0;
									const g = parseInt(safe.slice(2, 4), 16) || 0;
									const b = parseInt(safe.slice(4, 6), 16) || 0;
									let a = 1;
									if (safe.length === 8) {
										a = (parseInt(safe.slice(6, 8), 16) || 255) / 255;
									}
									defaultValue = { r, g, b, a };
								} else if (newArgType === "Selection") {
									defaultValue = { expression: newArgExpression };
								} else if (newArgType === "Palette") {
									defaultValue = newArgPalette as unknown as Record<
										string,
										unknown
									>;
								} else if (newArgType === "Gradient") {
									defaultValue = newArgGradient as unknown as Record<
										string,
										unknown
									>;
								} else {
									defaultValue = newArgScalar as unknown as Record<
										string,
										unknown
									>;
								}

								const newArg: PatternArgDef = {
									id,
									name: normalizedArgName || "arg",
									argType: newArgType,
									defaultValue,
								};

								let nextArgs: PatternArgDef[];
								if (editingArgId) {
									nextArgs = patternArgs.map((a) =>
										a.id === editingArgId ? newArg : a,
									);
								} else {
									nextArgs = [...patternArgs, newArg];
								}

								setPatternArgs(nextArgs);
								setArgDialogOpen(false);
								setEditingArgId(null);
								setNewArgName("");
								setNewArgColor("#ff0000");
								setNewArgScalar(1.0);
								setNewArgExpression("all");
								setNewArgPalette({ colors: ["#ff0080", "#00ffc8", "#ffbe28"] });
								setNewArgGradient({
									stops: [
										{ color: "#000000", t: 0 },
										{ color: "#ffffff", t: 1 },
									],
								});
								setNewArgType("Color");

								const graph = serializeGraph();
								if (graph && editorRef.current) {
									const withNode = withPatternArgsNode(
										{ ...graph, args: nextArgs },
										nextArgs,
									);
									editorRef.current.loadGraph(withNode, getNodeDefinitions);
									void executeGraph(withNode);
								}
							}}
							className="px-3 py-2 text-sm font-medium bg-primary text-primary-foreground rounded-md"
						>
							{editingArgId ? "Save Changes" : "Add Arg"}
						</button>
					</DialogFooter>
				</DialogContent>
			</Dialog>

			{/* Unsaved changes confirmation */}
			<Dialog
				open={blocker.state === "blocked"}
				onOpenChange={(open) => {
					if (!open && blocker.state === "blocked") blocker.reset();
				}}
			>
				<DialogContent showCloseButton={false}>
					<DialogHeader>
						<DialogTitle>Unsaved changes</DialogTitle>
						<DialogDescription>
							You have unsaved changes to this pattern. Do you want to save
							before leaving?
						</DialogDescription>
					</DialogHeader>
					<DialogFooter>
						<Button
							onClick={() => {
								if (blocker.state === "blocked") blocker.proceed();
							}}
						>
							Discard
						</Button>
						<Button
							onClick={async () => {
								await saveGraph();
								if (blocker.state === "blocked") blocker.proceed();
							}}
						>
							Save
						</Button>
					</DialogFooter>
				</DialogContent>
			</Dialog>

			<AlertDialog
				open={pendingDeleteArgId !== null}
				onOpenChange={(open) => {
					if (!open) setPendingDeleteArgId(null);
				}}
			>
				<AlertDialogContent>
					<AlertDialogHeader>
						<AlertDialogTitle>Delete Argument</AlertDialogTitle>
						<AlertDialogDescription>
							Are you sure you want to delete this argument?
						</AlertDialogDescription>
					</AlertDialogHeader>
					<AlertDialogFooter>
						<AlertDialogCancel>Cancel</AlertDialogCancel>
						<AlertDialogAction
							className="bg-destructive text-white hover:bg-destructive/90"
							onClick={confirmDeleteArg}
						>
							Delete
						</AlertDialogAction>
					</AlertDialogFooter>
				</AlertDialogContent>
			</AlertDialog>
		</>
	);
}
