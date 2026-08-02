import { Upload } from "lucide-react";
import { useCallback, useState } from "react";
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
import { invoke } from "@/shared/lib/tauri";
import { useTrackEditorStore } from "../stores/use-track-editor-store";
import {
	materializeTrackScores,
	trackScoreSnapshot,
} from "../utils/materialize-track-scores";

type ImportDslDialogProps = {
	open: boolean;
	onOpenChange: (open: boolean) => void;
};

export function ImportDslDialog({ open, onOpenChange }: ImportDslDialogProps) {
	const beatGrid = useTrackEditorStore((s) => s.beatGrid);
	const trackId = useTrackEditorStore((s) => s.trackId);
	const scoreId = useTrackEditorStore((s) => s.scoreId);
	const patterns = useTrackEditorStore((s) => s.patterns);
	const patternArgs = useTrackEditorStore((s) => s.patternArgs);
	const annotations = useTrackEditorStore((s) => s.annotations);
	const reloadAnnotations = useTrackEditorStore((s) => s.reloadAnnotations);

	const [text, setText] = useState("");
	const [errors, setErrors] = useState<string[]>([]);
	const [importing, setImporting] = useState(false);

	const handleImport = useCallback(async () => {
		if (!beatGrid || !trackId || !scoreId || text.trim() === "") return;

		const registry = buildRegistry(patterns, patternArgs);
		const result = parse(text, registry, {
			beatsPerBar: beatGrid?.beatsPerBar ?? 4,
		});

		if (!result.ok) {
			setErrors(result.errors.map((e) => formatError(e, text)));
			return;
		}

		setErrors([]);
		setImporting(true);

		try {
			const newAnnotations = dslToAnnotations(
				result.document,
				beatGrid,
				patterns,
				patternArgs,
			);

			const baseScores = trackScoreSnapshot(annotations);
			const replacement = materializeTrackScores(
				newAnnotations,
				baseScores,
				scoreId,
			);
			await invoke("replace_track_scores", {
				scoreId,
				trackId,
				baseScores,
				scores: replacement,
			});
			await reloadAnnotations();

			setText("");
			onOpenChange(false);
		} catch (error) {
			setErrors([
				error instanceof Error ? error.message : "Failed to import score",
			]);
		} finally {
			setImporting(false);
		}
	}, [
		text,
		beatGrid,
		trackId,
		scoreId,
		patterns,
		patternArgs,
		annotations,
		reloadAnnotations,
		onOpenChange,
	]);

	return (
		<Dialog
			open={open}
			onOpenChange={(next) => {
				if (!next) {
					setErrors([]);
				}
				onOpenChange(next);
			}}
		>
			<DialogContent className="sm:max-w-2xl">
				<DialogHeader>
					<DialogTitle>Import DSL</DialogTitle>
					<DialogDescription>
						Paste a DSL score below. This will replace all existing annotations.
					</DialogDescription>
				</DialogHeader>
				<textarea
					value={text}
					onChange={(e) => {
						setText(e.target.value);
						if (errors.length > 0) setErrors([]);
					}}
					placeholder={
						"solid_color(all) @1-5 color=#ff0000\nsolid_color(all) @5-9 color=#0000ff"
					}
					className="h-80 w-full resize-none rounded-md border bg-muted/50 p-3 font-mono text-sm leading-relaxed focus:outline-none"
				/>
				{errors.length > 0 && (
					<pre className="max-h-40 overflow-auto rounded-md border border-destructive/30 bg-destructive/5 p-3 font-mono text-xs text-destructive">
						{errors.join("\n\n")}
					</pre>
				)}
				<DialogFooter>
					<Button onClick={() => onOpenChange(false)}>Cancel</Button>
					<Button
						disabled={text.trim() === "" || importing}
						onClick={() => void handleImport()}
					>
						<Upload className="size-4" />
						{importing ? "Importing..." : "Import"}
					</Button>
				</DialogFooter>
			</DialogContent>
		</Dialog>
	);
}
