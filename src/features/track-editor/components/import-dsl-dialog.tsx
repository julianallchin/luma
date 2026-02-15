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
import { useTrackEditorStore } from "../stores/use-track-editor-store";

type ImportDslDialogProps = {
	open: boolean;
	onOpenChange: (open: boolean) => void;
};

export function ImportDslDialog({ open, onOpenChange }: ImportDslDialogProps) {
	const beatGrid = useTrackEditorStore((s) => s.beatGrid);
	const patterns = useTrackEditorStore((s) => s.patterns);
	const patternArgs = useTrackEditorStore((s) => s.patternArgs);
	const annotations = useTrackEditorStore((s) => s.annotations);
	const deleteAnnotations = useTrackEditorStore((s) => s.deleteAnnotations);
	const createAnnotation = useTrackEditorStore((s) => s.createAnnotation);

	const [text, setText] = useState("");
	const [errors, setErrors] = useState<string[]>([]);
	const [importing, setImporting] = useState(false);

	const handleImport = useCallback(async () => {
		if (!beatGrid || text.trim() === "") return;

		const registry = buildRegistry(patterns, patternArgs);
		const result = parse(text, registry);

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

			// Delete all existing annotations
			if (annotations.length > 0) {
				await deleteAnnotations(annotations.map((a) => a.id));
			}

			// Create new annotations
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

			setText("");
			onOpenChange(false);
		} finally {
			setImporting(false);
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
					placeholder={"@1-4\nsolid_color(all) color=#ff0000\n\n@5-8\nhold"}
					className="h-80 w-full resize-none rounded-md border bg-muted/50 p-3 font-mono text-sm leading-relaxed focus:outline-none"
				/>
				{errors.length > 0 && (
					<pre className="max-h-40 overflow-auto rounded-md border border-destructive/30 bg-destructive/5 p-3 font-mono text-xs text-destructive">
						{errors.join("\n\n")}
					</pre>
				)}
				<DialogFooter>
					<Button
						variant="outline"
						size="sm"
						onClick={() => onOpenChange(false)}
					>
						Cancel
					</Button>
					<Button
						size="sm"
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
