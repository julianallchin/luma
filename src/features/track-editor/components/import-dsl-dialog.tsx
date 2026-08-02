import { Upload } from "lucide-react";
import { useCallback, useState } from "react";
import { importScoreDsl, validateScoreDsl } from "@/lib/dsl";
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

type ImportDslDialogProps = {
	open: boolean;
	onOpenChange: (open: boolean) => void;
};

export function ImportDslDialog({ open, onOpenChange }: ImportDslDialogProps) {
	const trackId = useTrackEditorStore((state) => state.trackId);
	const scoreId = useTrackEditorStore((state) => state.scoreId);
	const venueId = useTrackEditorStore((state) => state.venueId);
	const reloadAnnotations = useTrackEditorStore(
		(state) => state.reloadAnnotations,
	);

	const [text, setText] = useState("");
	const [errors, setErrors] = useState<string[]>([]);
	const [importing, setImporting] = useState(false);

	const handleImport = useCallback(async () => {
		if (
			trackId === null ||
			scoreId === null ||
			venueId === null ||
			text.trim() === ""
		)
			return;

		setErrors([]);
		setImporting(true);
		try {
			const scope = { scoreId, trackId, venueId };
			const validation = await validateScoreDsl(scope, text);
			if (!validation.valid) {
				setErrors(
					validation.diagnostics
						.filter((diagnostic) => diagnostic.severity === "error")
						.map((diagnostic) => diagnostic.formatted),
				);
				return;
			}

			// Import compiles and validates the complete source again inside the
			// authoritative Git + projection transaction. The check above exists
			// only to present source-located diagnostics before that mutation.
			await importScoreDsl(scope, text, validation.baseRevision);
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
	}, [text, trackId, scoreId, venueId, reloadAnnotations, onOpenChange]);

	return (
		<Dialog
			open={open}
			onOpenChange={(next) => {
				if (!next) setErrors([]);
				onOpenChange(next);
			}}
		>
			<DialogContent className="sm:max-w-2xl">
				<DialogHeader>
					<DialogTitle>Import DSL</DialogTitle>
					<DialogDescription>
						Paste a DSL score below. This creates a restorable Git commit and
						replaces the current score.
					</DialogDescription>
				</DialogHeader>
				<textarea
					value={text}
					onChange={(event) => {
						setText(event.target.value);
						if (errors.length > 0) setErrors([]);
					}}
					placeholder="solid_color(all) @1-5 color=#ff0000\nsolid_color(all) @5-9 color=#0000ff"
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
