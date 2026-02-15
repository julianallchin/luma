import { Check, Copy } from "lucide-react";
import { useCallback, useMemo, useState } from "react";
import { annotationsToDsl } from "@/lib/dsl/convert";
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

type ExportDslDialogProps = {
	open: boolean;
	onOpenChange: (open: boolean) => void;
};

export function ExportDslDialog({ open, onOpenChange }: ExportDslDialogProps) {
	const annotations = useTrackEditorStore((s) => s.annotations);
	const beatGrid = useTrackEditorStore((s) => s.beatGrid);
	const patterns = useTrackEditorStore((s) => s.patterns);
	const patternArgs = useTrackEditorStore((s) => s.patternArgs);

	const [copied, setCopied] = useState(false);

	const dslText = useMemo(() => {
		if (!open || !beatGrid) return "";
		return annotationsToDsl(annotations, beatGrid, patterns, patternArgs);
	}, [open, annotations, beatGrid, patterns, patternArgs]);

	const barCount = beatGrid?.downbeats.length ?? 0;

	const handleCopy = useCallback(async () => {
		await navigator.clipboard.writeText(dslText);
		setCopied(true);
		setTimeout(() => setCopied(false), 2000);
	}, [dslText]);

	return (
		<Dialog open={open} onOpenChange={onOpenChange}>
			<DialogContent className="sm:max-w-2xl">
				<DialogHeader>
					<DialogTitle>Export DSL</DialogTitle>
					<DialogDescription>
						{annotations.length} annotation
						{annotations.length !== 1 ? "s" : ""} across {barCount} bar
						{barCount !== 1 ? "s" : ""}
					</DialogDescription>
				</DialogHeader>
				<textarea
					readOnly
					value={dslText}
					className="h-80 w-full resize-none rounded-md border bg-muted/50 p-3 font-mono text-sm leading-relaxed focus:outline-none"
				/>
				<DialogFooter>
					<Button variant="outline" size="sm" onClick={() => void handleCopy()}>
						{copied ? (
							<Check className="size-4" />
						) : (
							<Copy className="size-4" />
						)}
						{copied ? "Copied" : "Copy"}
					</Button>
				</DialogFooter>
			</DialogContent>
		</Dialog>
	);
}
