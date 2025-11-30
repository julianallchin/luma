import * as React from "react";
import type { NodeProps } from "reactflow";
import { useGraphStore } from "@/features/patterns/stores/use-graph-store";
import { cn } from "@/shared/lib/utils";
import { BaseNode } from "./base-node";
import type { BaseNodeData } from "./types";

// Define frequency bins with unique IDs
const FREQ_BINS = [
	{ id: "sub", label: "Sub", min: 20, max: 60, color: "bg-red-500" },
	{ id: "bass", label: "Bass", min: 60, max: 250, color: "bg-orange-500" },
	{ id: "low_mid", label: "Low Mid", min: 250, max: 500, color: "bg-yellow-500" },
	{ id: "mid", label: "Mid", min: 500, max: 2000, color: "bg-green-500" },
	{ id: "high_mid", label: "High Mid", min: 2000, max: 4000, color: "bg-emerald-500" },
	{ id: "pres", label: "Pres", min: 4000, max: 6000, color: "bg-cyan-500" },
	{ id: "brill", label: "Brill", min: 6000, max: 20000, color: "bg-blue-500" },
];

export function FrequencyAmplitudeNode(props: NodeProps<BaseNodeData>) {
	const { data, id } = props;
	const params = useGraphStore(
		(state) => state.nodeParams[id] ?? ({} as Record<string, unknown>),
	);
	const setParam = useGraphStore((state) => state.setParam);

    // Internal state for selected bin IDs
    const [selectedBinIds, setSelectedBinIds] = React.useState<string[]>(() => {
        try {
            const storedRangesJson = (params.selected_frequency_ranges as string) ?? "[]";
            const storedRanges: [number, number][] = JSON.parse(storedRangesJson);
            // Map stored ranges back to bin IDs
            return FREQ_BINS.filter(bin => 
                storedRanges.some(r => r[0] === bin.min && r[1] === bin.max)
            ).map(bin => bin.id);
        } catch {
            return [];
        }
    });

    // Effect to update the backend parameter when selectedBinIds changes
    React.useEffect(() => {
        const selectedRanges = FREQ_BINS
            .filter(bin => selectedBinIds.includes(bin.id))
            .map(bin => [bin.min, bin.max]);
        setParam(id, "selected_frequency_ranges", JSON.stringify(selectedRanges));
    }, [selectedBinIds, id, setParam]);

    // Calculate overall min/max frequency for display
    const overallMinFreq = React.useMemo(() => {
        if (selectedBinIds.length === 0) return 0;
        return Math.min(...FREQ_BINS.filter(bin => selectedBinIds.includes(bin.id)).map(bin => bin.min));
    }, [selectedBinIds]);

    const overallMaxFreq = React.useMemo(() => {
        if (selectedBinIds.length === 0) return 0;
        return Math.max(...FREQ_BINS.filter(bin => selectedBinIds.includes(bin.id)).map(bin => bin.max));
    }, [selectedBinIds]);

	const isBinActive = (binId: string) => selectedBinIds.includes(binId);

	const handleBinClick = (clickedBinId: string, multi: boolean) => {
		setSelectedBinIds((prevSelectedBinIds) => {
			const isAlreadySelected = prevSelectedBinIds.includes(clickedBinId);

			if (multi) {
				// Multi-select/toggle
				if (isAlreadySelected) {
					return prevSelectedBinIds.filter((binId) => binId !== clickedBinId);
				} else {
					return [...prevSelectedBinIds, clickedBinId];
				}
			} else {
				// Single select
				if (isAlreadySelected && prevSelectedBinIds.length === 1) {
                    return []; // Deselect if only one is selected
                }
				return [clickedBinId];
			}
		});
	};

	const controls = (
		<div className="flex flex-col gap-3 p-2 w-64">
            {/* Bins Visualizer */}
			<div className="flex h-24 w-full items-end gap-1 rounded-md bg-card/50 p-2 border border-border/50">
				{FREQ_BINS.map((bin) => {
					const active = isBinActive(bin.id);
					return (
						<button
							key={bin.id}
							type="button"
							className={cn(
								"group relative flex-1 rounded-sm transition-all duration-200 ease-out hover:opacity-90",
								active ? bin.color : "bg-secondary hover:bg-accent",
                                active ? "h-full" : "h-1/2" // Active bars are taller
							)}
							onClick={(e) => handleBinClick(bin.id, e.shiftKey)}
							title={`${bin.label}: ${bin.min}-${bin.max} Hz`}
						>
                            {/* Label overlay on hover or active */}
							<div className="absolute inset-0 flex items-end justify-center pb-1 opacity-0 transition-opacity group-hover:opacity-100">
                                <span className="text-[8px] font-bold uppercase tracking-tighter text-primary-foreground/80 rotate-[-90deg] whitespace-nowrap origin-bottom translate-y-[-4px]">
                                    {bin.label}
                                </span>
                            </div>
                            {/* Active Indicator Dot */}
                            {active && (
                                <div className="absolute top-1 left-1/2 -translate-x-1/2 h-1 w-1 rounded-full bg-white/50" />
                            )}
						</button>
					);
				})}
			</div>
            
            {/* Manual numeric / Info display */}
            <div className="flex justify-between text-[10px] text-muted-foreground font-mono">
                {selectedBinIds.length > 0 ? (
                    <>
                        <span>{overallMinFreq.toFixed(0)} Hz</span>
                        <span>-</span>
                        <span>{overallMaxFreq.toFixed(0)} Hz</span>
                    </>
                ) : (
                    <span>No Bins Selected</span>
                )}
            </div>
		</div>
	);

	return <BaseNode {...props} data={{ ...data, paramControls: controls }} />;
}
