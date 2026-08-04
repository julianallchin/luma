import {
	createModels,
	type ImageContent,
	type Message,
	type TextContent,
} from "@earendil-works/pi-ai";
import { googleProvider } from "@earendil-works/pi-ai/providers/google";
import { Sparkles, Square, Upload } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import type { FixtureGroup } from "@/bindings/groups";
import type { BeatGrid, ScoreSummary, TrackSummary } from "@/bindings/schema";
import { useAppViewStore } from "@/features/app/stores/use-app-view-store";
import { exportScoreDsl, importScoreDsl, validateScoreDsl } from "@/lib/dsl";
import { Button } from "@/shared/components/ui/button";
import {
	Dialog,
	DialogContent,
	DialogDescription,
	DialogFooter,
	DialogHeader,
	DialogTitle,
} from "@/shared/components/ui/dialog";
import {
	Select,
	SelectContent,
	SelectItem,
	SelectTrigger,
	SelectValue,
} from "@/shared/components/ui/select";
import { invoke } from "@/shared/lib/tauri";
import { useTrackEditorStore } from "../stores/use-track-editor-store";
import { buildGeneratePrompt } from "../utils/build-generate-prompt";

type GenerateDslDialogProps = {
	open: boolean;
	onOpenChange: (open: boolean) => void;
};

const ENV_API_KEY = import.meta.env.VITE_GEMINI_API_KEY as string | undefined;
const STORAGE_KEY = "luma:gemini-api-key";
const MODEL_STORAGE_KEY = "luma:gemini-model";

const GEMINI_MODELS = [
	{ id: "gemini-3.1-pro-preview", label: "Gemini 3.1 Pro" },
	{ id: "gemini-3-flash-preview", label: "Gemini 3 Flash" },
	{ id: "gemini-3.1-flash-lite-preview", label: "Gemini 3.1 Flash Lite" },
	{ id: "gemini-2.5-pro", label: "Gemini 2.5 Pro" },
] as const;

const DEFAULT_MODEL = GEMINI_MODELS[0].id;

type DslMessage =
	| {
			role: "user";
			content: string | (TextContent | ImageContent)[];
	  }
	| { role: "assistant"; content: string };

const googleModels = createModels();
googleModels.setProvider(googleProvider());

/** Build the bar-timestamp cheatsheet text. */
function buildCheatsheet(downbeats: number[], totalBars: number): string {
	const lines = downbeats.map((t, i) => {
		const m = Math.floor(t / 60);
		const s = t % 60;
		return `Bar ${i + 1} - ${m}:${s.toFixed(2).padStart(5, "0")}`;
	});
	return `${totalBars} bars total.\n\nBar-Timestamp Cheatsheet:\n${lines.join("\n")}`;
}

const ANALYZE_SYSTEM = `You are an expert lighting designer analyzing a track to plan a lighting score.

You are given an audio track, its bar-timestamp cheatsheet, and the finished DSL lighting score that was written for it.

Your job: work backwards from the music to write the analysis the designer would have done BEFORE writing the DSL. Do NOT reference any specific pattern names or DSL syntax. Focus purely on the music and high-level lighting intent.

Start by identifying the drops — the highest-energy moments. Then work backwards:
- Where are the drops? At which bars/timestamps?
- That means the builds/breakdowns leading into them start where?
- What does that tell you about the phrasing and song structure?
- Where are the intros, verses, choruses, bridges, outros?
- What are the dominant sounds in each section? (bass, vocals, synths, drums, FX)
- What is the mood/energy arc of the track?

Then describe the lighting design intent for each section in plain language:
- What should the overall vibe be? (dark, bright, chaotic, minimal, warm, cold)
- Where should the lighting build tension? Where should it release?
- Which sections need contrast vs continuity?
- Where should there be strobing/flashing vs smooth washes?
- What color palette fits each section?

Output ONLY plain text analysis. No DSL, no pattern names, no XML tags.`;

const ANALYZE_CURRENT_SYSTEM = `You are an expert lighting designer analyzing a track to plan a lighting score.

You are given an audio track and its bar-timestamp cheatsheet.

Analyze the track and write a lighting design plan. Do NOT reference any specific pattern names or DSL syntax. Focus purely on the music and high-level lighting intent.

Start by identifying the drops — the highest-energy moments. Then work backwards:
- Where are the drops? At which bars/timestamps?
- That means the builds/breakdowns leading into them start where?
- What does that tell you about the phrasing and song structure?
- Where are the intros, verses, choruses, bridges, outros?
- What are the dominant sounds in each section? (bass, vocals, synths, drums, FX)
- What is the mood/energy arc of the track?

Then describe the lighting design intent for each section in plain language:
- What should the overall vibe be? (dark, bright, chaotic, minimal, warm, cold)
- Where should the lighting build tension? Where should it release?
- Which sections need contrast vs continuity?
- Where should there be strobing/flashing vs smooth washes?
- What color palette fits each section?

Output ONLY plain text analysis. No DSL, no pattern names, no XML tags.`;

function getApiKey(inputKey: string): string | null {
	if (ENV_API_KEY) return ENV_API_KEY;
	if (inputKey.trim()) return inputKey.trim();
	const stored = localStorage.getItem(STORAGE_KEY);
	if (stored) return stored;
	return null;
}

/** Fetch an exemplar track's audio, beats, and DSL by its ID. */
async function fetchExemplar(
	exemplarTrackId: string,
	venueId: string,
): Promise<{
	audio: { data: string; mimeType: string };
	beats: BeatGrid;
	dsl: string;
} | null> {
	const [audio, beats, scores] = await Promise.all([
		invoke<{ data: string; mimeType: string }>("get_track_audio_base64", {
			trackId: exemplarTrackId,
		}),
		invoke<BeatGrid | null>("get_track_beats", { trackId: exemplarTrackId }),
		invoke<ScoreSummary[]>("list_scores_for_track", {
			trackId: exemplarTrackId,
			venueId,
		}),
	]);

	if (!beats || scores.length === 0) return null;
	const score = scores.find((candidate) => candidate.annotationCount > 0);
	if (!score) return null;
	const { source: dsl } = await exportScoreDsl(
		{ scoreId: score.id, trackId: exemplarTrackId, venueId },
		false,
	);
	if (!dsl.trim()) return null;

	return { audio, beats, dsl };
}

/** Fetch all tracks that have at least one annotation (score). */
async function fetchAnnotatedTracks(
	excludeTrackId: string | null,
	venueId: string,
): Promise<TrackSummary[]> {
	const allTracks = await invoke<TrackSummary[]>("list_tracks");
	const results: TrackSummary[] = [];
	for (const t of allTracks) {
		if (t.id === excludeTrackId) continue;
		const scores = await invoke<ScoreSummary[]>("list_scores_for_track", {
			trackId: t.id,
			venueId,
		});
		if (scores.some((score) => score.annotationCount > 0)) results.push(t);
	}
	return results;
}

export function GenerateDslDialog({
	open,
	onOpenChange,
}: GenerateDslDialogProps) {
	const trackId = useTrackEditorStore((s) => s.trackId);
	const venueId = useTrackEditorStore((s) => s.venueId);
	const scoreId = useTrackEditorStore((s) => s.scoreId);
	const beatGrid = useTrackEditorStore((s) => s.beatGrid);
	const patterns = useTrackEditorStore((s) => s.patterns);
	const patternArgs = useTrackEditorStore((s) => s.patternArgs);
	const reloadAnnotations = useTrackEditorStore((s) => s.reloadAnnotations);

	const [apiKeyInput, setApiKeyInput] = useState(
		() => localStorage.getItem(STORAGE_KEY) ?? "",
	);
	const [modelId, setModelId] = useState(
		() => localStorage.getItem(MODEL_STORAGE_KEY) ?? DEFAULT_MODEL,
	);
	const [exemplarAnalysis, setExemplarAnalysis] = useState(
		() => localStorage.getItem("luma:exemplar-analysis") ?? "",
	);
	const [annotatedTracks, setAnnotatedTracks] = useState<TrackSummary[]>([]);
	const [exemplarTrackId, setExemplarTrackId] = useState<string | null>(null);
	const [hints, setHints] = useState("");
	const [currentAnalysis, setCurrentAnalysis] = useState("");
	const [text, setText] = useState("");
	const [errors, setErrors] = useState<string[]>([]);
	const [generating, setGenerating] = useState(false);
	const [loading, setLoading] = useState(false);
	const abortRef = useRef<AbortController | null>(null);

	const systemRef = useRef<string>("");
	const messagesRef = useRef<DslMessage[]>([]);
	const exemplarRef = useRef<Awaited<ReturnType<typeof fetchExemplar>>>(null);

	// Load annotated tracks when dialog opens
	useEffect(() => {
		if (!open) return;
		if (venueId === null) return;
		fetchAnnotatedTracks(trackId, venueId).then((tracks) => {
			setAnnotatedTracks(tracks);
			// Auto-select first if nothing selected
			if (tracks.length > 0 && exemplarTrackId === null) {
				setExemplarTrackId(tracks[0].id);
			}
		});
	}, [open, trackId]); // eslint-disable-line react-hooks/exhaustive-deps

	const streamFromModel = useCallback(
		async (
			apiKey: string,
			model: string,
			system: string,
			messages: DslMessage[],
			signal: AbortSignal,
			onChunk: (fullText: string) => void,
		) => {
			const activeModel = googleModels.getModel("google", model);
			if (!activeModel || activeModel.api !== "google-generative-ai") {
				throw new Error(`Unknown Gemini model '${model}'.`);
			}
			const piMessages: Message[] = messages.map((message) => {
				if (message.role === "user") {
					return { ...message, timestamp: Date.now() };
				}
				return {
					role: "assistant",
					content: [{ type: "text", text: message.content }],
					api: activeModel.api,
					provider: activeModel.provider,
					model: activeModel.id,
					usage: {
						input: 0,
						output: 0,
						cacheRead: 0,
						cacheWrite: 0,
						totalTokens: 0,
						cost: {
							input: 0,
							output: 0,
							cacheRead: 0,
							cacheWrite: 0,
							total: 0,
						},
					},
					stopReason: "stop",
					timestamp: Date.now(),
				};
			});
			const result = googleModels.streamSimple(
				activeModel,
				{ systemPrompt: system, messages: piMessages },
				{ apiKey, signal },
			);

			let fullText = "";
			for await (const event of result) {
				if (signal.aborted) break;
				if (event.type === "text_delta") {
					fullText += event.delta;
					onChunk(fullText);
				} else if (event.type === "error") {
					if (event.reason === "aborted") {
						throw new DOMException("Generation aborted.", "AbortError");
					}
					throw new Error(event.error.errorMessage ?? "Gemini request failed.");
				}
			}
			return fullText;
		},
		[],
	);

	const ensureExemplar = useCallback(async () => {
		if (exemplarRef.current || exemplarTrackId === null || venueId === null)
			return;
		try {
			exemplarRef.current = await fetchExemplar(exemplarTrackId, venueId);
		} catch {
			// best-effort
		}
	}, [exemplarTrackId, venueId]);

	/** Validate API key and save if needed. Returns key or null. */
	const resolveApiKey = useCallback(() => {
		const apiKey = getApiKey(apiKeyInput);
		if (!apiKey) {
			setErrors(["Please enter a Gemini API key."]);
			return null;
		}
		if (!ENV_API_KEY && apiKeyInput.trim()) {
			localStorage.setItem(STORAGE_KEY, apiKeyInput.trim());
		}
		return apiKey;
	}, [apiKeyInput]);

	// ── Step 1: Analyze Exemplar ─────────────────────────────────
	// Send exemplar audio + DSL → model reverse-engineers the
	// analysis. No pattern names, drop-first reasoning.
	const handleAnalyzeExemplar = useCallback(async () => {
		if (!beatGrid || trackId === null) return;
		const apiKey = resolveApiKey();
		if (!apiKey) return;

		setErrors([]);
		setGenerating(true);
		setExemplarAnalysis("");

		const abort = new AbortController();
		abortRef.current = abort;

		try {
			if (exemplarTrackId === null) {
				setErrors(["Please select an exemplar track."]);
				setGenerating(false);
				return;
			}
			// Reset cached exemplar data and re-fetch
			exemplarRef.current = null;
			await ensureExemplar();
			const ex = exemplarRef.current as Awaited<
				ReturnType<typeof fetchExemplar>
			>;
			if (!ex) {
				setErrors(["Exemplar track has no beats or annotations."]);
				return;
			}

			const cheatsheet = buildCheatsheet(
				ex.beats.downbeats,
				ex.beats.downbeats.length,
			);

			const userMessage: DslMessage = {
				role: "user",
				content: [
					{
						type: "image",
						data: ex.audio.data,
						mimeType: ex.audio.mimeType,
					},
					{
						type: "text",
						text: `Here is a track and the lighting score that was written for it.\n\n${cheatsheet}\n\nDSL score:\n${ex.dsl}`,
					},
				],
			};

			const fullText = await streamFromModel(
				apiKey,
				modelId,
				ANALYZE_SYSTEM,
				[userMessage],
				abort.signal,
				(t) => setExemplarAnalysis(t),
			);
			localStorage.setItem("luma:exemplar-analysis", fullText);
		} catch (err: unknown) {
			if (!(err instanceof Error && err.name === "AbortError")) {
				setErrors([err instanceof Error ? err.message : "Analysis failed"]);
			}
		} finally {
			setGenerating(false);
			abortRef.current = null;
		}
	}, [
		trackId,
		beatGrid,
		exemplarTrackId,
		apiKeyInput,
		modelId,
		streamFromModel,
		ensureExemplar,
		resolveApiKey,
	]);

	// ── Step 2: Analyze Current Track ────────────────────────────
	// Few-shot: exemplar audio → exemplar analysis (from step 1)
	// Then current audio → model produces current analysis
	const handleAnalyzeCurrent = useCallback(async () => {
		if (!beatGrid || trackId === null) return;
		const apiKey = resolveApiKey();
		if (!apiKey) return;

		setErrors([]);
		setGenerating(true);
		setCurrentAnalysis("");

		const abort = new AbortController();
		abortRef.current = abort;

		try {
			const { data, mimeType } = await invoke<{
				data: string;
				mimeType: string;
			}>("get_track_audio_base64", { trackId });

			// Few-shot: exemplar audio → exemplar analysis
			const fewShotMessages: DslMessage[] = [];
			const ex = exemplarRef.current;
			const exAnalysis = exemplarAnalysis.trim();
			if (ex && exAnalysis) {
				const exCheatsheet = buildCheatsheet(
					ex.beats.downbeats,
					ex.beats.downbeats.length,
				);
				fewShotMessages.push(
					{
						role: "user",
						content: [
							{
								type: "image",
								data: ex.audio.data,
								mimeType: ex.audio.mimeType,
							},
							{
								type: "text",
								text: `Analyze this track and plan the lighting design.\n\n${exCheatsheet}`,
							},
						],
					},
					{
						role: "assistant",
						content: exAnalysis,
					},
				);
			}

			const currentCheatsheet = buildCheatsheet(
				beatGrid.downbeats,
				beatGrid.downbeats.length,
			);
			const hintsText = hints.trim();
			const userText = hintsText
				? `Analyze this track and plan the lighting design.\n\nHints from the user:\n${hintsText}\n\n${currentCheatsheet}`
				: `Analyze this track and plan the lighting design.\n\n${currentCheatsheet}`;
			const userMessage: DslMessage = {
				role: "user",
				content: [
					{ type: "image", data, mimeType },
					{ type: "text", text: userText },
				],
			};

			await streamFromModel(
				apiKey,
				modelId,
				ANALYZE_CURRENT_SYSTEM,
				[...fewShotMessages, userMessage],
				abort.signal,
				(t) => setCurrentAnalysis(t),
			);
		} catch (err: unknown) {
			if (!(err instanceof Error && err.name === "AbortError")) {
				setErrors([err instanceof Error ? err.message : "Analysis failed"]);
			}
		} finally {
			setGenerating(false);
			abortRef.current = null;
		}
	}, [
		trackId,
		beatGrid,
		exemplarAnalysis,
		hints,
		apiKeyInput,
		modelId,
		streamFromModel,
		resolveApiKey,
	]);

	// ── Step 3: Generate DSL ─────────────────────────────────────
	// Few-shot: exemplar audio → <analysis>…</analysis><dsl>…</dsl>
	// Current: audio + prefilled <analysis> → model continues with <dsl>
	const handleGenerate = useCallback(async () => {
		if (!beatGrid || trackId === null) return;
		const apiKey = resolveApiKey();
		if (!apiKey) return;

		setErrors([]);
		setGenerating(true);
		setText("");

		const abort = new AbortController();
		abortRef.current = abort;

		try {
			const { data, mimeType } = await invoke<{
				data: string;
				mimeType: string;
			}>("get_track_audio_base64", { trackId });

			const currentVenueId =
				useAppViewStore.getState().currentVenue?.id ?? null;
			let groupNames: string[] = [];
			if (currentVenueId) {
				const groups = await invoke<FixtureGroup[]>("list_groups", {
					venueId: currentVenueId,
				});
				groupNames = groups
					.map((g) => g.name)
					.filter((n): n is string => Boolean(n));
			}

			const system = buildGeneratePrompt(
				patterns,
				patternArgs,
				beatGrid.downbeats.length,
				beatGrid.downbeats,
				groupNames,
			);
			systemRef.current = system;

			const generateSystem = `${system}

## Instructions

Output ONLY the DSL text. No markdown fences, no explanation, no commentary.`;

			// Few-shot: exemplar audio → prefilled analysis + DSL
			const fewShotMessages: DslMessage[] = [];
			const ex = exemplarRef.current;
			const exAnalysis = exemplarAnalysis.trim();
			if (ex && exAnalysis) {
				const exCheatsheet = buildCheatsheet(
					ex.beats.downbeats,
					ex.beats.downbeats.length,
				);
				fewShotMessages.push(
					{
						role: "user",
						content: [
							{
								type: "image",
								data: ex.audio.data,
								mimeType: ex.audio.mimeType,
							},
							{
								type: "text",
								text: `Create a complete lighting score for this track.\n\n${exCheatsheet}`,
							},
						],
					},
					{
						role: "assistant",
						content: `<analysis>\n${exAnalysis}\n</analysis>\n${ex.dsl}`,
					},
				);
			}

			// Current track: audio + cheatsheet, analysis prefilled
			const currentCheatsheet = buildCheatsheet(
				beatGrid.downbeats,
				beatGrid.downbeats.length,
			);
			const curAnalysis = currentAnalysis.trim();
			const userMessage: DslMessage = {
				role: "user",
				content: [
					{ type: "image", data, mimeType },
					{
						type: "text",
						text: `Create a complete lighting score for this track.\n\n${currentCheatsheet}`,
					},
				],
			};

			const messages: DslMessage[] = [...fewShotMessages, userMessage];

			// Prefill assistant with the current analysis so it continues with DSL
			if (curAnalysis) {
				messages.push({
					role: "assistant",
					content: `<analysis>\n${curAnalysis}\n</analysis>\n`,
				});
			}

			const fullText = await streamFromModel(
				apiKey,
				modelId,
				generateSystem,
				messages,
				abort.signal,
				(t) => setText(t),
			);

			messagesRef.current = [
				userMessage,
				{ role: "assistant", content: fullText },
			];
		} catch (err: unknown) {
			if (!(err instanceof Error && err.name === "AbortError")) {
				setErrors([err instanceof Error ? err.message : "Generation failed"]);
			}
		} finally {
			setGenerating(false);
			abortRef.current = null;
		}
	}, [
		trackId,
		beatGrid,
		patterns,
		patternArgs,
		exemplarAnalysis,
		currentAnalysis,
		apiKeyInput,
		modelId,
		streamFromModel,
		resolveApiKey,
	]);

	const handleAbort = useCallback(() => {
		abortRef.current?.abort();
	}, []);

	const handleCheck = useCallback(async () => {
		if (
			!beatGrid ||
			text.trim() === "" ||
			scoreId === null ||
			trackId === null ||
			venueId === null
		)
			return;

		const dslText = text;
		let result: Awaited<ReturnType<typeof validateScoreDsl>>;
		try {
			result = await validateScoreDsl({ scoreId, trackId, venueId }, dslText);
		} catch (error) {
			setErrors([
				error instanceof Error ? error.message : "Failed to validate score",
			]);
			return;
		}

		if (result.valid) {
			setErrors(["Valid! No errors found."]);
			return;
		}

		const errorStrings = result.diagnostics
			.filter((diagnostic) => diagnostic.severity === "error")
			.map((diagnostic) => diagnostic.formatted);

		if (messagesRef.current.length > 0 && systemRef.current) {
			const apiKey = getApiKey(apiKeyInput);
			if (!apiKey) {
				setErrors(errorStrings);
				return;
			}

			setErrors([]);
			setGenerating(true);
			setText("");

			const abort = new AbortController();
			abortRef.current = abort;

			try {
				const fixMessage: DslMessage = {
					role: "user",
					content: `The DSL you produced has parse errors. Fix them and output the complete corrected DSL. Output ONLY the DSL text inside <dsl> tags, no explanations.\n\nErrors:\n${errorStrings.join("\n\n")}`,
				};

				const prevMessages = [...messagesRef.current];
				prevMessages[prevMessages.length - 1] = {
					role: "assistant",
					content: text,
				};
				const messages = [...prevMessages, fixMessage];

				const fullText = await streamFromModel(
					apiKey,
					modelId,
					systemRef.current,
					messages,
					abort.signal,
					(t) => setText(t),
				);

				messagesRef.current = [
					...messages,
					{ role: "assistant", content: fullText },
				];
			} catch (err: unknown) {
				if (!(err instanceof Error && err.name === "AbortError")) {
					setErrors([
						err instanceof Error ? err.message : "Fix generation failed",
					]);
				}
			} finally {
				setGenerating(false);
				abortRef.current = null;
			}
		} else {
			setErrors(errorStrings);
		}
	}, [
		text,
		beatGrid,
		scoreId,
		trackId,
		venueId,
		apiKeyInput,
		modelId,
		streamFromModel,
	]);

	const handleLoad = useCallback(async () => {
		if (
			!beatGrid ||
			trackId === null ||
			scoreId === null ||
			venueId === null ||
			text.trim() === ""
		)
			return;

		const dslText = text;

		setErrors([]);
		setLoading(true);

		try {
			const scope = { scoreId, trackId, venueId };
			const validation = await validateScoreDsl(scope, dslText);
			if (!validation.valid) {
				setErrors(
					validation.diagnostics
						.filter((diagnostic) => diagnostic.severity === "error")
						.map((diagnostic) => diagnostic.formatted),
				);
				return;
			}
			await importScoreDsl(scope, dslText, validation.baseRevision);
			await reloadAnnotations();
			onOpenChange(false);
		} catch (error) {
			setErrors([
				error instanceof Error ? error.message : "Failed to apply score",
			]);
		} finally {
			setLoading(false);
		}
	}, [
		text,
		trackId,
		venueId,
		beatGrid,
		scoreId,
		reloadAnnotations,
		onOpenChange,
	]);

	const showApiKeyInput = !ENV_API_KEY;
	const totalBars = beatGrid?.downbeats.length ?? 0;
	const hasExemplarAnalysis = exemplarAnalysis.trim() !== "";
	const hasCurrentAnalysis = currentAnalysis.trim() !== "";

	return (
		<Dialog
			open={open}
			onOpenChange={(next) => {
				if (!next) {
					if (generating) handleAbort();
					setErrors([]);
				}
				onOpenChange(next);
			}}
		>
			<DialogContent className="sm:max-w-2xl max-h-[90vh] overflow-y-auto">
				<DialogHeader>
					<DialogTitle>Generate Lighting Score</DialogTitle>
					<DialogDescription>
						Analyze an exemplar track, analyze the current track, then generate
						DSL ({totalBars} bars).
					</DialogDescription>
				</DialogHeader>

				<div className="flex gap-2">
					<Select
						value={modelId}
						onValueChange={(v) => {
							setModelId(v);
							localStorage.setItem(MODEL_STORAGE_KEY, v);
						}}
					>
						<SelectTrigger className="w-52">
							<SelectValue />
						</SelectTrigger>
						<SelectContent>
							{GEMINI_MODELS.map((m) => (
								<SelectItem key={m.id} value={m.id}>
									{m.label}
								</SelectItem>
							))}
						</SelectContent>
					</Select>
					{showApiKeyInput && (
						<input
							type="password"
							value={apiKeyInput}
							onChange={(e) => setApiKeyInput(e.target.value)}
							placeholder="Gemini API key"
							className="flex-1 rounded-md border bg-muted/50 px-3 py-2 text-sm focus:outline-none"
						/>
					)}
				</div>

				{/* Step 1: Exemplar analysis */}
				<div className="space-y-1">
					<div className="flex items-center justify-between">
						<div className="flex items-center gap-2">
							<span className="text-xs font-medium text-muted-foreground whitespace-nowrap">
								1. Exemplar
							</span>
							<Select
								value={exemplarTrackId !== null ? String(exemplarTrackId) : ""}
								onValueChange={(v) => {
									setExemplarTrackId(v);
									exemplarRef.current = null;
									setExemplarAnalysis("");
									localStorage.removeItem("luma:exemplar-analysis");
								}}
							>
								<SelectTrigger className="h-6 w-48 text-xs">
									<SelectValue placeholder="Select track..." />
								</SelectTrigger>
								<SelectContent>
									{annotatedTracks.map((t) => (
										<SelectItem key={t.id} value={String(t.id)}>
											{t.artist
												? `${t.artist} — ${t.title ?? "Untitled"}`
												: (t.title ?? "Untitled")}
										</SelectItem>
									))}
								</SelectContent>
							</Select>
						</div>
						<Button
							className="h-6 px-2 text-xs"
							onClick={() => void handleAnalyzeExemplar()}
							disabled={
								trackId === null ||
								!beatGrid ||
								exemplarTrackId === null ||
								generating
							}
						>
							<Sparkles className="size-3" />
							Analyze
						</Button>
					</div>
					<textarea
						value={exemplarAnalysis}
						onChange={(e) => {
							if (!generating) {
								setExemplarAnalysis(e.target.value);
								localStorage.setItem("luma:exemplar-analysis", e.target.value);
							}
						}}
						readOnly={generating}
						placeholder="Reverse-engineers the lighting design intent from the exemplar track..."
						className="h-32 w-full resize-none rounded-md border bg-muted/50 p-3 text-sm leading-relaxed focus:outline-none"
					/>
				</div>

				{/* Step 2: Current track analysis */}
				<div className="space-y-1">
					<div className="flex items-center justify-between">
						<span className="text-xs font-medium text-muted-foreground">
							2. Current Track Analysis
						</span>
						<Button
							className="h-6 px-2 text-xs"
							onClick={() => void handleAnalyzeCurrent()}
							disabled={
								trackId === null ||
								!beatGrid ||
								!hasExemplarAnalysis ||
								generating
							}
						>
							<Sparkles className="size-3" />
							Analyze
						</Button>
					</div>
					<textarea
						ref={(el) => {
							if (el) {
								el.style.height = "auto";
								el.style.height = `${el.scrollHeight}px`;
							}
						}}
						value={hints}
						onChange={(e) => {
							setHints(e.target.value);
							e.target.style.height = "auto";
							e.target.style.height = `${e.target.scrollHeight}px`;
						}}
						rows={1}
						placeholder="Hints: e.g. drops at bar 17 and 49, breakdown at bar 33..."
						className="w-full resize-none rounded-md border bg-muted/50 px-3 py-1.5 text-xs focus:outline-none"
					/>
					<textarea
						value={currentAnalysis}
						onChange={(e) => {
							if (!generating) setCurrentAnalysis(e.target.value);
						}}
						readOnly={generating}
						placeholder="Analyzes the current track structure and plans lighting design..."
						className="h-32 w-full resize-none rounded-md border bg-muted/50 p-3 text-sm leading-relaxed focus:outline-none"
					/>
				</div>

				{/* Step 3: DSL output */}
				<div className="space-y-1">
					<div className="flex items-center justify-between">
						<span className="text-xs font-medium text-muted-foreground">
							3. DSL
						</span>
						<Button
							className="h-6 px-2 text-xs"
							onClick={() => void handleGenerate()}
							disabled={
								trackId === null ||
								!beatGrid ||
								!hasCurrentAnalysis ||
								generating
							}
						>
							<Sparkles className="size-3" />
							Generate
						</Button>
					</div>
					<textarea
						value={text}
						onChange={(e) => {
							if (!generating) {
								setText(e.target.value);
								if (errors.length > 0) setErrors([]);
							}
						}}
						readOnly={generating}
						placeholder="DSL will be generated using the exemplar + current analysis..."
						className="h-48 w-full resize-none rounded-md border bg-muted/50 p-3 font-mono text-sm leading-relaxed focus:outline-none"
					/>
				</div>

				{errors.length > 0 && (
					<pre className="max-h-40 overflow-auto rounded-md border border-destructive/30 bg-destructive/5 p-3 font-mono text-xs text-destructive">
						{errors.join("\n\n")}
					</pre>
				)}

				<DialogFooter className="gap-2 sm:gap-0">
					{generating && (
						<Button onClick={handleAbort}>
							<Square className="size-4" />
							Abort
						</Button>
					)}
					<Button
						onClick={() => void handleCheck()}
						disabled={text.trim() === "" || generating}
					>
						Check
					</Button>
					<Button
						onClick={() => void handleLoad()}
						disabled={text.trim() === "" || generating || loading}
					>
						<Upload className="size-4" />
						{loading ? "Loading..." : "Load"}
					</Button>
				</DialogFooter>
			</DialogContent>
		</Dialog>
	);
}
