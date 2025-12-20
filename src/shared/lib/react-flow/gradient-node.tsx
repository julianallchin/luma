import * as React from "react";
import { type NodeProps, useEdges } from "reactflow";

import { useGraphStore } from "@/features/patterns/stores/use-graph-store";
import {
	ColorPicker,
	ColorPickerAlpha,
	ColorPickerHue,
	ColorPickerSelection,
} from "@/shared/components/ui/shadcn-io/color-picker";
import { BaseNode } from "./base-node";
import type { BaseNodeData } from "./types";

// Helper to parse hex color string to display value
function hexToDisplay(hex: string): string {
	if (hex.startsWith("#")) return hex;
	return `#${hex}`;
}

// Helper to convert RGBA array to hex
function rgbaToHex(rgba: number[]): string {
	if (rgba.length < 3) return "#000000";
	const r = Math.round(rgba[0]).toString(16).padStart(2, "0");
	const g = Math.round(rgba[1]).toString(16).padStart(2, "0");
	const b = Math.round(rgba[2]).toString(16).padStart(2, "0");
	if (rgba.length >= 4 && rgba[3] !== 1) {
		const a = Math.round(rgba[3] * 255)
			.toString(16)
			.padStart(2, "0");
		return `#${r}${g}${b}${a}`;
	}
	return `#${r}${g}${b}`;
}

// Helper to parse color JSON param to hex
function colorParamToHex(colorParam: string | undefined): string | null {
	if (!colorParam) return null;
	try {
		const parsed = JSON.parse(colorParam);
		if (
			typeof parsed.r === "number" &&
			typeof parsed.g === "number" &&
			typeof parsed.b === "number"
		) {
			const r = Math.round(parsed.r).toString(16).padStart(2, "0");
			const g = Math.round(parsed.g).toString(16).padStart(2, "0");
			const b = Math.round(parsed.b).toString(16).padStart(2, "0");
			return `#${r}${g}${b}`;
		}
	} catch {
		// Not JSON, might be hex already
		if (colorParam.startsWith("#")) return colorParam;
	}
	return null;
}

export function GradientNode(props: NodeProps<BaseNodeData>) {
	const { data, id } = props;
	const edges = useEdges();
	const nodeParams = useGraphStore((state) => state.nodeParams);
	const params = nodeParams[id] ?? ({} as Record<string, unknown>);
	const setParam = useGraphStore((state) => state.setParam);

	// Check if color ports are connected
	const startColorEdge = edges.find(
		(edge) => edge.target === id && edge.targetHandle === "start_color",
	);
	const endColorEdge = edges.find(
		(edge) => edge.target === id && edge.targetHandle === "end_color",
	);

	const hasStartColorInput = !!startColorEdge;
	const hasEndColorInput = !!endColorEdge;

	// Get start color param (hex string) for when not connected
	const startColorParam =
		(params.start_color as string) ??
		data.definition.params.find((p) => p.id === "start_color")?.defaultText ??
		"#000000";

	// Get end color param (hex string) for when not connected
	const endColorParam =
		(params.end_color as string) ??
		data.definition.params.find((p) => p.id === "end_color")?.defaultText ??
		"#ffffff";

	// Try to get connected node's color for preview
	const getConnectedColor = (
		edge: (typeof edges)[0] | undefined,
	): string | null => {
		if (!edge) return null;
		const sourceNodeParams = nodeParams[edge.source];
		if (!sourceNodeParams) return null;
		// Try to read 'color' param (from color nodes)
		const colorParam = sourceNodeParams.color as string | undefined;
		return colorParamToHex(colorParam);
	};

	const connectedStartColor = getConnectedColor(startColorEdge);
	const connectedEndColor = getConnectedColor(endColorEdge);

	// Determine colors for preview
	const previewStartColor = hasStartColorInput
		? (connectedStartColor ?? "var(--muted)")
		: hexToDisplay(startColorParam);
	const previewEndColor = hasEndColorInput
		? (connectedEndColor ?? "var(--muted)")
		: hexToDisplay(endColorParam);

	const handleStartColorChange = React.useCallback(
		(rgba: unknown) => {
			if (Array.isArray(rgba) && rgba.length >= 3) {
				const hex = rgbaToHex(rgba as number[]);
				setParam(id, "start_color", hex);
			}
		},
		[id, setParam],
	);

	const handleEndColorChange = React.useCallback(
		(rgba: unknown) => {
			if (Array.isArray(rgba) && rgba.length >= 3) {
				const hex = rgbaToHex(rgba as number[]);
				setParam(id, "end_color", hex);
			}
		},
		[id, setParam],
	);

	// Create gradient preview
	const gradientStyle = {
		background: `linear-gradient(to right, ${previewStartColor}, ${previewEndColor})`,
	};

	const controls = (
		<div className="p-2 space-y-3 nodrag">
			{/* Gradient preview */}
			<div
				className="h-6 w-full rounded border border-border"
				style={gradientStyle}
			/>

			{/* Start Color - only show picker if not connected */}
			{!hasStartColorInput && (
				<div className="space-y-1">
					<span className="text-[10px] uppercase tracking-wide text-muted-foreground font-medium">
						Start
					</span>
					<ColorPicker
						defaultValue={hexToDisplay(startColorParam)}
						onChange={handleStartColorChange}
						className="w-full"
					>
						<div className="flex flex-col gap-1.5">
							<ColorPickerSelection className="h-20 w-full rounded" />
							<ColorPickerHue />
							<ColorPickerAlpha />
						</div>
					</ColorPicker>
				</div>
			)}

			{/* End Color - only show picker if not connected */}
			{!hasEndColorInput && (
				<div className="space-y-1">
					<span className="text-[10px] uppercase tracking-wide text-muted-foreground font-medium">
						End
					</span>
					<ColorPicker
						defaultValue={hexToDisplay(endColorParam)}
						onChange={handleEndColorChange}
						className="w-full"
					>
						<div className="flex flex-col gap-1.5">
							<ColorPickerSelection className="h-20 w-full rounded" />
							<ColorPickerHue />
							<ColorPickerAlpha />
						</div>
					</ColorPicker>
				</div>
			)}

			{/* Show indicator when both are connected */}
			{hasStartColorInput && hasEndColorInput && (
				<div className="text-[10px] text-center text-muted-foreground italic">
					Colors from inputs
				</div>
			)}
		</div>
	);

	return <BaseNode {...props} data={{ ...data, paramControls: controls }} />;
}
