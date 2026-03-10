import { Check, CheckCircle2, Copy, XCircle } from "lucide-react";
import { useCallback, useMemo, useState } from "react";
import {
	annotationsToDsl,
	buildRegistry,
	type DslAnnotation,
	dslToAnnotations,
} from "@/lib/dsl/convert";
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
import type { TimelineAnnotation } from "../stores/use-track-editor-store";
import { useTrackEditorStore } from "../stores/use-track-editor-store";

type ExportDslDialogProps = {
	open: boolean;
	onOpenChange: (open: boolean) => void;
};

type VerifyResult =
	| { status: "pass"; message: string }
	| { status: "fail"; message: string; diffs: string[] };

export function ExportDslDialog({ open, onOpenChange }: ExportDslDialogProps) {
	const annotations = useTrackEditorStore((s) => s.annotations);
	const beatGrid = useTrackEditorStore((s) => s.beatGrid);
	const patterns = useTrackEditorStore((s) => s.patterns);
	const patternArgs = useTrackEditorStore((s) => s.patternArgs);

	const [copied, setCopied] = useState(false);
	const [verifyResult, setVerifyResult] = useState<VerifyResult | null>(null);

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

	const handleVerify = useCallback(() => {
		if (!beatGrid || dslText.trim() === "") {
			setVerifyResult({
				status: "fail",
				message: "No DSL to verify",
				diffs: [],
			});
			return;
		}

		// Parse DSL back to annotations
		const registry = buildRegistry(patterns, patternArgs);
		const parseResult = parse(dslText, registry, {
			beatsPerBar: beatGrid.beatsPerBar,
		});

		if (!parseResult.ok) {
			setVerifyResult({
				status: "fail",
				message: "DSL failed to parse",
				diffs: parseResult.errors.map((e) => e.message),
			});
			return;
		}

		const reimported = dslToAnnotations(
			parseResult.document,
			beatGrid,
			patterns,
			patternArgs,
		);

		// Build z-index normalization: original z-values sorted → 0,1,2,...
		const origZValues = [...new Set(annotations.map((a) => a.zIndex))].sort(
			(a, b) => a - b,
		);
		const zMap = new Map(origZValues.map((z, i) => [z, i]));

		// Group by normalized layer
		const origByLayer = new Map<number, TimelineAnnotation[]>();
		for (const a of annotations) {
			const norm = zMap.get(a.zIndex) ?? a.zIndex;
			if (!origByLayer.has(norm)) origByLayer.set(norm, []);
			origByLayer.get(norm)?.push(a);
		}
		const reimByLayer = new Map<number, DslAnnotation[]>();
		for (const a of reimported) {
			if (!reimByLayer.has(a.zIndex)) reimByLayer.set(a.zIndex, []);
			reimByLayer.get(a.zIndex)?.push(a);
		}

		const patternNameMap = new Map(patterns.map((p) => [p.id, p.name]));
		const diffs: string[] = [];
		const TIME_TOL = 0.02;

		if (reimported.length !== annotations.length) {
			diffs.push(
				`Count mismatch: ${annotations.length} original → ${reimported.length} reimported`,
			);
		}

		for (const [layer, origAnns] of origByLayer) {
			const reimAnns = reimByLayer.get(layer) ?? [];
			const origSorted = [...origAnns].sort(
				(a, b) => a.startTime - b.startTime,
			);
			const reimSorted = [...reimAnns].sort(
				(a, b) => a.startTime - b.startTime,
			);

			if (origSorted.length !== reimSorted.length) {
				diffs.push(
					`Layer ${layer}: ${origSorted.length} → ${reimSorted.length} annotations`,
				);
				continue;
			}

			for (let i = 0; i < origSorted.length; i++) {
				const orig = origSorted[i];
				const reim = reimSorted[i];
				const name =
					patternNameMap.get(orig.patternId) ?? String(orig.patternId);
				const label = `L${layer}[${i}] ${name}`;

				if (orig.patternId !== reim.patternId) {
					diffs.push(
						`${label}: patternId ${orig.patternId} → ${reim.patternId}`,
					);
				}
				if (Math.abs(orig.startTime - reim.startTime) > TIME_TOL) {
					diffs.push(
						`${label}: start ${orig.startTime.toFixed(3)} → ${reim.startTime.toFixed(3)}`,
					);
				}
				if (Math.abs(orig.endTime - reim.endTime) > TIME_TOL) {
					diffs.push(
						`${label}: end ${orig.endTime.toFixed(3)} → ${reim.endTime.toFixed(3)}`,
					);
				}
				if (orig.blendMode !== reim.blendMode) {
					diffs.push(`${label}: blend ${orig.blendMode} → ${reim.blendMode}`);
				}

				// Compare args per pattern arg defs
				const argDefs = patternArgs[orig.patternId] ?? [];
				const origArgs = (orig.args ?? {}) as Record<string, unknown>;
				const reimArgs = reim.args;

				for (const def of argDefs) {
					if (def.argType === "Selection") {
						const origExpr =
							(origArgs[def.id] as { expression?: string })?.expression ??
							"all";
						const reimExpr =
							(reimArgs[def.id] as { expression?: string })?.expression ??
							"all";
						if (origExpr !== reimExpr) {
							diffs.push(`${label}: selection "${origExpr}" → "${reimExpr}"`);
						}
						continue;
					}

					const ov = origArgs[def.id];
					const rv = reimArgs[def.id];

					if (def.argType === "Color") {
						const oc = ov as {
							r: number;
							g: number;
							b: number;
							a?: number;
						} | null;
						const rc = rv as {
							r: number;
							g: number;
							b: number;
							a?: number;
						} | null;
						if (oc && rc) {
							if (
								Math.abs(oc.r - rc.r) > 1 ||
								Math.abs(oc.g - rc.g) > 1 ||
								Math.abs(oc.b - rc.b) > 1 ||
								Math.abs((oc.a ?? 1) - (rc.a ?? 1)) > 0.01
							) {
								diffs.push(
									`${label}: ${def.name} color(${oc.r},${oc.g},${oc.b}) → (${rc.r},${rc.g},${rc.b})`,
								);
							}
						}
						continue;
					}

					if (typeof ov === "number" && typeof rv === "number") {
						if (Math.abs(ov - rv) > 0.001) {
							diffs.push(`${label}: ${def.name} ${ov} → ${rv}`);
						}
					} else if (ov != null && rv == null) {
						diffs.push(
							`${label}: ${def.name} ${JSON.stringify(ov)} → undefined`,
						);
					}
				}
			}
		}

		if (diffs.length === 0) {
			setVerifyResult({
				status: "pass",
				message: `Roundtrip OK: ${annotations.length} annotations across ${origZValues.length} layers`,
			});
		} else {
			setVerifyResult({
				status: "fail",
				message: `${diffs.length} difference${diffs.length !== 1 ? "s" : ""} found`,
				diffs,
			});
		}
	}, [dslText, beatGrid, annotations, patterns, patternArgs]);

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
				{verifyResult && (
					<div
						className={`flex items-start gap-2 rounded-md border p-3 text-xs font-mono ${
							verifyResult.status === "pass"
								? "border-green-500/30 bg-green-500/5 text-green-600 dark:text-green-400"
								: "border-destructive/30 bg-destructive/5 text-destructive"
						}`}
					>
						{verifyResult.status === "pass" ? (
							<CheckCircle2 className="size-4 shrink-0 mt-0.5" />
						) : (
							<XCircle className="size-4 shrink-0 mt-0.5" />
						)}
						<div className="min-w-0 flex-1">
							<div>{verifyResult.message}</div>
							{verifyResult.status === "fail" &&
								verifyResult.diffs.length > 0 && (
									<pre
										className="mt-2 max-h-32 overflow-auto whitespace-pre-wrap select-text cursor-text"
										onClick={(e) => {
											const sel = window.getSelection();
											if (sel?.isCollapsed) {
												const range = document.createRange();
												range.selectNodeContents(e.currentTarget);
												sel.removeAllRanges();
												sel.addRange(range);
											}
										}}
										onKeyDown={() => {}}
									>
										{verifyResult.diffs.join("\n")}
									</pre>
								)}
						</div>
						{verifyResult.status === "fail" &&
							verifyResult.diffs.length > 0 && (
								<button
									type="button"
									className="shrink-0 mt-0.5 hover:opacity-70"
									title="Copy diffs"
									onClick={() =>
										void navigator.clipboard.writeText(
											verifyResult.diffs.join("\n"),
										)
									}
								>
									<Copy className="size-3.5" />
								</button>
							)}
					</div>
				)}
				<DialogFooter className="gap-2 sm:gap-0">
					<Button
						variant="outline"
						size="sm"
						onClick={handleVerify}
						disabled={dslText.trim() === ""}
					>
						Verify Roundtrip
					</Button>
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
