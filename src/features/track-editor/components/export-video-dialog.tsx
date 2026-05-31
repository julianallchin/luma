import { save } from "@tauri-apps/plugin-dialog";
import { FileVideo } from "lucide-react";
import { useCallback, useState } from "react";
import { toast } from "sonner";
import type { StageExportHandle } from "@/features/visualizer/components/stage-visualizer";
import { Button } from "@/shared/components/ui/button";
import {
	Dialog,
	DialogContent,
	DialogDescription,
	DialogFooter,
	DialogHeader,
	DialogTitle,
} from "@/shared/components/ui/dialog";
import { Label } from "@/shared/components/ui/label";
import {
	Select,
	SelectContent,
	SelectItem,
	SelectTrigger,
	SelectValue,
} from "@/shared/components/ui/select";
import { runExport } from "../export/run-export";
import { useExportStore } from "../export/use-export-store";

type ExportVideoDialogProps = {
	open: boolean;
	onOpenChange: (open: boolean) => void;
	trackId: string;
	venueId: string;
	trackName: string;
	exportHandleRef: React.MutableRefObject<StageExportHandle | null>;
};

type Preset = { label: string; width: number; height: number };

const RESOLUTION_PRESETS: Preset[] = [
	{ label: "720p (1280×720)", width: 1280, height: 720 },
	{ label: "1080p (1920×1080)", width: 1920, height: 1080 },
	{ label: "1440p (2560×1440)", width: 2560, height: 1440 },
];

const FPS_OPTIONS = [24, 30, 60];

export function ExportVideoDialog({
	open,
	onOpenChange,
	trackId,
	venueId,
	trackName,
	exportHandleRef,
}: ExportVideoDialogProps) {
	const [presetIdx, setPresetIdx] = useState(1);
	const [fps, setFps] = useState(30);
	const isExporting = useExportStore((s) => s.isExporting);
	const currentFrame = useExportStore((s) => s.currentFrame);
	const totalFrames = useExportStore((s) => s.totalFrames);
	const status = useExportStore((s) => s.status);
	const requestCancel = useExportStore((s) => s.requestCancel);

	const handleExport = useCallback(async () => {
		const preset = RESOLUTION_PRESETS[presetIdx];
		const defaultName = `${trackName || trackId}.mp4`.replace(
			/[\\/:*?"<>|]/g,
			"_",
		);
		const outputPath = await save({
			title: "Export light show",
			defaultPath: defaultName,
			filters: [{ name: "MP4 video", extensions: ["mp4"] }],
		});
		if (!outputPath) return;

		const handle = exportHandleRef.current;
		if (!handle) {
			toast.error("Visualizer not ready yet");
			return;
		}

		try {
			await runExport({
				trackId,
				venueId,
				outputPath,
				fps,
				width: preset.width,
				height: preset.height,
				handle,
			});
			toast.success(`Exported to ${outputPath}`);
			onOpenChange(false);
		} catch (err) {
			if (err instanceof Error && err.message === "Export cancelled") {
				toast.message("Export cancelled");
			} else {
				console.error(err);
				toast.error(
					`Export failed: ${err instanceof Error ? err.message : String(err)}`,
				);
			}
		}
	}, [
		presetIdx,
		fps,
		trackId,
		venueId,
		trackName,
		exportHandleRef,
		onOpenChange,
	]);

	const progressPct =
		totalFrames > 0 ? Math.floor((currentFrame / totalFrames) * 100) : 0;

	return (
		<Dialog open={open} onOpenChange={isExporting ? undefined : onOpenChange}>
			<DialogContent className="sm:max-w-md">
				<DialogHeader>
					<DialogTitle className="flex items-center gap-2">
						<FileVideo className="h-4 w-4" />
						Export light show video
					</DialogTitle>
					<DialogDescription>
						Renders the current score at the saved camera shot, including
						volumetric haze and bloom. Output is H.264 MP4 with muxed track
						audio.
					</DialogDescription>
				</DialogHeader>

				<div className="space-y-4 py-2">
					<div className="space-y-2">
						<Label>Resolution</Label>
						<Select
							disabled={isExporting}
							value={String(presetIdx)}
							onValueChange={(v) => setPresetIdx(Number(v))}
						>
							<SelectTrigger>
								<SelectValue />
							</SelectTrigger>
							<SelectContent>
								{RESOLUTION_PRESETS.map((p, i) => (
									<SelectItem key={p.label} value={String(i)}>
										{p.label}
									</SelectItem>
								))}
							</SelectContent>
						</Select>
					</div>

					<div className="space-y-2">
						<Label>Frame rate</Label>
						<Select
							disabled={isExporting}
							value={String(fps)}
							onValueChange={(v) => setFps(Number(v))}
						>
							<SelectTrigger>
								<SelectValue />
							</SelectTrigger>
							<SelectContent>
								{FPS_OPTIONS.map((f) => (
									<SelectItem key={f} value={String(f)}>
										{f} fps
									</SelectItem>
								))}
							</SelectContent>
						</Select>
					</div>

					{isExporting && (
						<div className="space-y-1.5 pt-2">
							<div className="flex justify-between text-xs text-muted-foreground">
								<span>{status}</span>
								<span>
									{currentFrame}/{totalFrames} ({progressPct}%)
								</span>
							</div>
							<div className="h-1.5 w-full rounded-full bg-muted overflow-hidden">
								<div
									className="h-full bg-primary transition-[width] duration-100"
									style={{ width: `${progressPct}%` }}
								/>
							</div>
						</div>
					)}
				</div>

				<DialogFooter>
					{isExporting ? (
						<Button variant="outline" onClick={requestCancel}>
							Cancel
						</Button>
					) : (
						<>
							<Button variant="ghost" onClick={() => onOpenChange(false)}>
								Close
							</Button>
							<Button onClick={handleExport}>Start export</Button>
						</>
					)}
				</DialogFooter>
			</DialogContent>
		</Dialog>
	);
}
