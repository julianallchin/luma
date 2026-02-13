import { createGoogleGenerativeAI } from "@ai-sdk/google";
import { invoke } from "@tauri-apps/api/core";
import type { ModelMessage } from "ai";
import { streamText } from "ai";
import { Sparkles, Square, Upload } from "lucide-react";
import { useCallback, useRef, useState } from "react";
import { buildRegistry, dslToAnnotations } from "@/lib/dsl/convert";
import { formatError } from "@/lib/dsl/errors";
import { parse } from "@/lib/dsl/parser";
import { Button } from "@/shared/components/ui/button";
import {
	Dialog,
	DialogContent,
	DialogDescription,
	DialogFooter,
	DialogHeader,
	DialogTitle,
} from "@/shared/components/ui/dialog";
import { useTrackEditorStore } from "../stores/use-track-editor-store";
import { buildGeneratePrompt } from "../utils/build-generate-prompt";

type GenerateDslDialogProps = {
	open: boolean;
	onOpenChange: (open: boolean) => void;
};

const ENV_API_KEY = import.meta.env.VITE_GEMINI_API_KEY as string | undefined;
const STORAGE_KEY = "luma:gemini-api-key";

function getApiKey(inputKey: string): string | null {
	if (ENV_API_KEY) return ENV_API_KEY;
	if (inputKey.trim()) return inputKey.trim();
	const stored = localStorage.getItem(STORAGE_KEY);
	if (stored) return stored;
	return null;
}

export function GenerateDslDialog({
	open,
	onOpenChange,
}: GenerateDslDialogProps) {
	const trackId = useTrackEditorStore((s) => s.trackId);
	const beatGrid = useTrackEditorStore((s) => s.beatGrid);
	const patterns = useTrackEditorStore((s) => s.patterns);
	const patternArgs = useTrackEditorStore((s) => s.patternArgs);
	const annotations = useTrackEditorStore((s) => s.annotations);
	const deleteAnnotations = useTrackEditorStore((s) => s.deleteAnnotations);
	const createAnnotation = useTrackEditorStore((s) => s.createAnnotation);

	const [apiKeyInput, setApiKeyInput] = useState(
		() => localStorage.getItem(STORAGE_KEY) ?? "",
	);
	const [text, setText] = useState("");
	const [errors, setErrors] = useState<string[]>([]);
	const [generating, setGenerating] = useState(false);
	const [loading, setLoading] = useState(false);
	const abortRef = useRef<AbortController | null>(null);

	// Conversation state for multi-turn (generate → check/fix)
	const systemRef = useRef<string>("");
	const messagesRef = useRef<ModelMessage[]>([]);

	const streamFromModel = useCallback(
		async (
			apiKey: string,
			system: string,
			messages: ModelMessage[],
			signal: AbortSignal,
		) => {
			const google = createGoogleGenerativeAI({ apiKey });
			const result = streamText({
				model: google("gemini-2.5-pro"),
				system,
				messages,
				abortSignal: signal,
			});

			let fullText = "";
			for await (const chunk of result.textStream) {
				if (signal.aborted) break;
				fullText += chunk;
				setText(fullText);
			}
			return fullText;
		},
		[],
	);

	const handleGenerate = useCallback(async () => {
		if (!beatGrid || trackId === null) return;

		const apiKey = getApiKey(apiKeyInput);
		if (!apiKey) {
			setErrors(["Please enter a Gemini API key."]);
			return;
		}

		if (!ENV_API_KEY && apiKeyInput.trim()) {
			localStorage.setItem(STORAGE_KEY, apiKeyInput.trim());
		}

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

			const system = buildGeneratePrompt(
				patterns,
				patternArgs,
				beatGrid.downbeats.length,
			);
			systemRef.current = system;

			const userMessage: ModelMessage = {
				role: "user",
				content: [
					{ type: "file", data, mediaType: mimeType },
					{
						type: "text",
						text: "Create a complete lighting score for this track. Output ONLY the DSL text.",
					},
				],
			};
			const messages: ModelMessage[] = [userMessage];

			const fullText = await streamFromModel(
				apiKey,
				system,
				messages,
				abort.signal,
			);

			// Store conversation for follow-up fixes
			messagesRef.current = [
				userMessage,
				{ role: "assistant", content: fullText },
			];
		} catch (err: unknown) {
			if (err instanceof Error && err.name === "AbortError") {
				// User aborted — keep partial text
			} else {
				const msg = err instanceof Error ? err.message : "Generation failed";
				setErrors([msg]);
			}
		} finally {
			setGenerating(false);
			abortRef.current = null;
		}
	}, [trackId, beatGrid, patterns, patternArgs, apiKeyInput, streamFromModel]);

	const handleAbort = useCallback(() => {
		abortRef.current?.abort();
	}, []);

	const handleCheck = useCallback(async () => {
		if (!beatGrid || text.trim() === "") return;

		const registry = buildRegistry(patterns, patternArgs);
		const result = parse(text, registry);

		if (result.ok) {
			setErrors(["Valid! No errors found."]);
			return;
		}

		const errorStrings = result.errors.map((e) => formatError(e, text));

		// If we have conversation context, send errors to the model for a fix
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
				const fixMessage: ModelMessage = {
					role: "user",
					content: `The DSL you produced has parse errors. Fix them and output the complete corrected DSL. Output ONLY the DSL text, no explanations.\n\nErrors:\n${errorStrings.join("\n\n")}`,
				};

				// Update conversation: replace last assistant message with actual text, add fix request
				const prevMessages = [...messagesRef.current];
				prevMessages[prevMessages.length - 1] = {
					role: "assistant",
					content: text,
				};
				const messages = [...prevMessages, fixMessage];

				const fullText = await streamFromModel(
					apiKey,
					systemRef.current,
					messages,
					abort.signal,
				);

				messagesRef.current = [
					...messages,
					{ role: "assistant", content: fullText },
				];
			} catch (err: unknown) {
				if (err instanceof Error && err.name === "AbortError") {
					// keep partial
				} else {
					const msg =
						err instanceof Error ? err.message : "Fix generation failed";
					setErrors([msg]);
				}
			} finally {
				setGenerating(false);
				abortRef.current = null;
			}
		} else {
			// No conversation context — just show the errors
			setErrors(errorStrings);
		}
	}, [text, beatGrid, patterns, patternArgs, apiKeyInput, streamFromModel]);

	const handleLoad = useCallback(async () => {
		if (!beatGrid || text.trim() === "") return;

		const registry = buildRegistry(patterns, patternArgs);
		const result = parse(text, registry);
		if (!result.ok) {
			setErrors(result.errors.map((e) => formatError(e, text)));
			return;
		}

		setErrors([]);
		setLoading(true);

		try {
			const newAnnotations = dslToAnnotations(
				result.document,
				beatGrid,
				patterns,
				patternArgs,
			);

			if (annotations.length > 0) {
				await deleteAnnotations(annotations.map((a) => a.id));
			}

			for (const ann of newAnnotations) {
				await createAnnotation({
					patternId: ann.patternId,
					startTime: ann.startTime,
					endTime: ann.endTime,
					zIndex: ann.zIndex,
					blendMode: ann.blendMode,
					args: ann.args,
				});
			}

			onOpenChange(false);
		} finally {
			setLoading(false);
		}
	}, [
		text,
		beatGrid,
		patterns,
		patternArgs,
		annotations,
		deleteAnnotations,
		createAnnotation,
		onOpenChange,
	]);

	const showApiKeyInput = !ENV_API_KEY;
	const totalBars = beatGrid?.downbeats.length ?? 0;

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
			<DialogContent className="sm:max-w-2xl">
				<DialogHeader>
					<DialogTitle>Generate Lighting Score</DialogTitle>
					<DialogDescription>
						Send the track audio to Gemini to generate a complete lighting score
						({totalBars} bars).
					</DialogDescription>
				</DialogHeader>

				{showApiKeyInput && (
					<input
						type="password"
						value={apiKeyInput}
						onChange={(e) => setApiKeyInput(e.target.value)}
						placeholder="Gemini API key"
						className="w-full rounded-md border bg-muted/50 px-3 py-2 text-sm focus:outline-none"
					/>
				)}

				<textarea
					value={text}
					onChange={(e) => {
						if (!generating) {
							setText(e.target.value);
							if (errors.length > 0) setErrors([]);
						}
					}}
					readOnly={generating}
					placeholder="Generated DSL will appear here..."
					className="h-80 w-full resize-none rounded-md border bg-muted/50 p-3 font-mono text-sm leading-relaxed focus:outline-none"
				/>

				{errors.length > 0 && (
					<pre className="max-h-40 overflow-auto rounded-md border border-destructive/30 bg-destructive/5 p-3 font-mono text-xs text-destructive">
						{errors.join("\n\n")}
					</pre>
				)}

				<DialogFooter className="gap-2 sm:gap-0">
					{generating ? (
						<Button size="sm" variant="destructive" onClick={handleAbort}>
							<Square className="size-4" />
							Abort
						</Button>
					) : (
						<Button
							size="sm"
							onClick={() => void handleGenerate()}
							disabled={trackId === null || !beatGrid}
						>
							<Sparkles className="size-4" />
							Generate
						</Button>
					)}
					<Button
						size="sm"
						variant="outline"
						onClick={() => void handleCheck()}
						disabled={text.trim() === "" || generating}
					>
						Check
					</Button>
					<Button
						size="sm"
						variant="outline"
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
