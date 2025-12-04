import { useEffect, useState } from "react";
import { useTrackEditorStore } from "../stores/use-track-editor-store";
import { Input } from "@/shared/components/ui/input";

export function InspectorPanel() {
	const selectedAnnotationId = useTrackEditorStore(
		(s) => s.selectedAnnotationId,
	);
	const annotations = useTrackEditorStore((s) => s.annotations);
	const updateAnnotation = useTrackEditorStore((s) => s.updateAnnotation);

	const selectedAnnotation = annotations.find(
		(a) => a.id === selectedAnnotationId,
	);

	// Local state for inputs to avoid stuttering while typing
	const [startTime, setStartTime] = useState("");
	const [endTime, setEndTime] = useState("");
	const [zIndex, setZIndex] = useState("");

	// Sync local state when selection changes
	useEffect(() => {
		if (selectedAnnotation) {
			setStartTime(selectedAnnotation.startTime.toFixed(3));
			setEndTime(selectedAnnotation.endTime.toFixed(3));
			setZIndex(selectedAnnotation.zIndex.toString());
		}
	}, [selectedAnnotation]);

	if (!selectedAnnotation) {
		return (
			<div className="w-80 border-l border-neutral-800 bg-neutral-900/50 flex flex-col">
				<div className="h-12 border-b border-neutral-800 flex items-center px-4 font-medium text-sm text-neutral-400">
					Inspector
				</div>
				<div className="flex-1 p-8 flex items-center justify-center text-neutral-500 text-sm">
					Select a pattern to view details
				</div>
			</div>
		);
	}

	const handleBlur = () => {
		const start = parseFloat(startTime);
		const end = parseFloat(endTime);
		const z = parseInt(zIndex, 10);

		if (!isNaN(start) && !isNaN(end) && !isNaN(z)) {
			updateAnnotation({
				id: selectedAnnotation.id,
				startTime: start,
				endTime: end,
				zIndex: z,
			});
		}
	};

	return (
		<div className="w-80 border-l border-neutral-800 bg-neutral-900/50 flex flex-col">
			<div className="h-12 border-b border-neutral-800 flex items-center px-4 font-medium text-sm text-neutral-200">
				Inspector
			</div>
			<div className="flex-1 p-4 space-y-6 overflow-y-auto">
				<div>
					<div className="text-xs font-semibold text-neutral-500 uppercase tracking-wider mb-3">
						Pattern Properties
					</div>

					<div className="space-y-4">
						<div className="space-y-1">
							<label className="text-xs text-neutral-400">Name</label>
							<div className="text-sm font-medium text-neutral-200 truncate">
								{selectedAnnotation.patternName ||
									`Pattern ${selectedAnnotation.patternId}`}
							</div>
						</div>

						<div className="space-y-1">
							<label className="text-xs text-neutral-400">Pattern ID</label>
							<div className="text-sm font-mono text-neutral-400">
								{selectedAnnotation.patternId}
							</div>
						</div>
					</div>
				</div>

				<div className="h-px bg-neutral-800" />

				<div>
					<div className="text-xs font-semibold text-neutral-500 uppercase tracking-wider mb-3">
						Timing & Layering
					</div>

					<div className="space-y-4">
						<div className="grid grid-cols-2 gap-2">
							<div className="space-y-1">
								<label className="text-xs text-neutral-400">Start (s)</label>
								<Input
									type="text"
									value={startTime}
									onChange={(e) => setStartTime(e.target.value)}
									onBlur={handleBlur}
									onKeyDown={(e) => e.key === "Enter" && handleBlur()}
								/>
							</div>
							<div className="space-y-1">
								<label className="text-xs text-neutral-400">End (s)</label>
								<input
									type="text"
									value={endTime}
									onChange={(e) => setEndTime(e.target.value)}
									onBlur={handleBlur}
									onKeyDown={(e) => e.key === "Enter" && handleBlur()}
									className="w-full bg-neutral-950 border border-neutral-800 rounded px-2 py-1 text-sm text-neutral-200 focus:outline-none focus:border-blue-500"
								/>
							</div>
						</div>

						<div className="space-y-1">
							<label className="text-xs text-neutral-400">
								Z-Index (Layer)
							</label>
							<div className="flex items-center gap-2">
								<input
									type="number"
									value={zIndex}
									onChange={(e) => setZIndex(e.target.value)}
									onBlur={handleBlur}
									onKeyDown={(e) => e.key === "Enter" && handleBlur()}
									className="w-full bg-neutral-950 border border-neutral-800 rounded px-2 py-1 text-sm text-neutral-200 focus:outline-none focus:border-blue-500"
								/>
								<div className="text-xs text-neutral-500 shrink-0">
									Higher = On Top
								</div>
							</div>
						</div>
					</div>
				</div>
			</div>
		</div>
	);
}
