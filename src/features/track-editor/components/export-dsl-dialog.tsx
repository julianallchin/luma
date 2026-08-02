import { Check, CheckCircle2, Copy, Loader2, XCircle } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { exportScoreDsl, validateScoreDsl } from "@/lib/dsl";
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

type VerifyResult =
	| { status: "pass"; message: string }
	| { status: "fail"; message: string; diagnostics: string[] };

export function ExportDslDialog({ open, onOpenChange }: ExportDslDialogProps) {
	const scoreId = useTrackEditorStore((state) => state.scoreId);
	const trackId = useTrackEditorStore((state) => state.trackId);
	const venueId = useTrackEditorStore((state) => state.venueId);
	const beatGrid = useTrackEditorStore((state) => state.beatGrid);

	const [dslText, setDslText] = useState("");
	const [clipCount, setClipCount] = useState(0);
	const [loading, setLoading] = useState(false);
	const [loadError, setLoadError] = useState<string | null>(null);
	const [copied, setCopied] = useState(false);
	const [verifying, setVerifying] = useState(false);
	const [verifyResult, setVerifyResult] = useState<VerifyResult | null>(null);

	useEffect(() => {
		if (!open || scoreId === null || trackId === null || venueId === null) {
			return;
		}
		let current = true;
		setLoading(true);
		setLoadError(null);
		setVerifyResult(null);
		exportScoreDsl({ scoreId, trackId, venueId }, true)
			.then((result) => {
				if (!current) return;
				setDslText(result.source);
				setClipCount(result.clipCount);
			})
			.catch((error: unknown) => {
				if (!current) return;
				setDslText("");
				setClipCount(0);
				setLoadError(
					error instanceof Error ? error.message : "Failed to export score",
				);
			})
			.finally(() => {
				if (current) setLoading(false);
			});
		return () => {
			current = false;
		};
	}, [open, scoreId, trackId, venueId]);

	const handleCopy = useCallback(async () => {
		await navigator.clipboard.writeText(dslText);
		setCopied(true);
		setTimeout(() => setCopied(false), 2000);
	}, [dslText]);

	const handleVerify = useCallback(async () => {
		if (
			scoreId === null ||
			trackId === null ||
			venueId === null ||
			dslText.trim() === ""
		) {
			setVerifyResult({
				status: "fail",
				message: "No DSL to verify",
				diagnostics: [],
			});
			return;
		}
		setVerifying(true);
		try {
			const result = await validateScoreDsl(
				{ scoreId, trackId, venueId },
				dslText,
			);
			if (result.valid) {
				setVerifyResult({
					status: "pass",
					message: `Rust roundtrip OK: ${result.clipCount ?? clipCount} clips`,
				});
			} else {
				setVerifyResult({
					status: "fail",
					message: `${result.diagnostics.length} validation error${result.diagnostics.length === 1 ? "" : "s"}`,
					diagnostics: result.diagnostics.map(
						(diagnostic) => diagnostic.formatted,
					),
				});
			}
		} catch (error) {
			setVerifyResult({
				status: "fail",
				message: error instanceof Error ? error.message : "Verification failed",
				diagnostics: [],
			});
		} finally {
			setVerifying(false);
		}
	}, [clipCount, dslText, scoreId, trackId, venueId]);

	const barCount = beatGrid?.downbeats.length ?? 0;

	return (
		<Dialog
			open={open}
			onOpenChange={(next) => {
				if (!next) setVerifyResult(null);
				onOpenChange(next);
			}}
		>
			<DialogContent className="sm:max-w-2xl">
				<DialogHeader>
					<DialogTitle>Export DSL</DialogTitle>
					<DialogDescription>
						{clipCount} annotation{clipCount === 1 ? "" : "s"} across {barCount}{" "}
						bar{barCount === 1 ? "" : "s"}
					</DialogDescription>
				</DialogHeader>
				<div className="relative">
					<textarea
						readOnly
						value={dslText}
						className="h-80 w-full resize-none rounded-md border bg-muted/50 p-3 font-mono text-sm leading-relaxed focus:outline-none"
					/>
					{loading && (
						<div className="absolute inset-0 flex items-center justify-center rounded-md bg-background/70">
							<Loader2 className="size-5 animate-spin text-muted-foreground" />
						</div>
					)}
				</div>
				{loadError && (
					<div className="rounded-md border border-destructive/30 bg-destructive/5 p-3 text-xs text-destructive">
						{loadError}
					</div>
				)}
				{verifyResult && (
					<div
						className={`flex items-start gap-2 rounded-md border p-3 text-xs font-mono ${
							verifyResult.status === "pass"
								? "border-green-500/30 bg-green-500/5 text-green-600 dark:text-green-400"
								: "border-destructive/30 bg-destructive/5 text-destructive"
						}`}
					>
						{verifyResult.status === "pass" ? (
							<CheckCircle2 className="mt-0.5 size-4 shrink-0" />
						) : (
							<XCircle className="mt-0.5 size-4 shrink-0" />
						)}
						<div className="min-w-0 flex-1">
							<div>{verifyResult.message}</div>
							{verifyResult.status === "fail" &&
								verifyResult.diagnostics.length > 0 && (
									<pre className="mt-2 max-h-32 overflow-auto whitespace-pre-wrap select-text">
										{verifyResult.diagnostics.join("\n\n")}
									</pre>
								)}
						</div>
					</div>
				)}
				<DialogFooter className="gap-2 sm:gap-0">
					<Button
						onClick={() => void handleVerify()}
						disabled={dslText.trim() === "" || loading || verifying}
					>
						{verifying && <Loader2 className="size-4 animate-spin" />}
						Verify Roundtrip
					</Button>
					<Button
						onClick={() => void handleCopy()}
						disabled={dslText.trim() === "" || loading}
					>
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
