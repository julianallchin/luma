import * as React from "react";
import type { NodeProps } from "reactflow";
import { useGraphStore } from "@/features/patterns/stores/use-graph-store";
import {
	ColorPicker,
	ColorPickerAlpha,
	ColorPickerHue,
	ColorPickerSelection,
} from "@/shared/components/ui/shadcn-io/color-picker";
import { BaseNode } from "./base-node";
import type { BaseNodeData } from "./types";

// Color node with color picker
export function ColorNode(props: NodeProps<BaseNodeData>) {
	const { data, id } = props;
	const params = useGraphStore(
		(state) => state.nodeParams[id] ?? ({} as Record<string, unknown>),
	);
	const setParam = useGraphStore((state) => state.setParam);

	// Parse color from JSON string stored in params
	const colorParam =
		(params.color as string) ??
		data.definition.params.find((p) => p.id === "color")?.defaultText ??
		'{"r":255,"g":0,"b":0,"a":1}';

	// Convert stored JSON to hex string for ColorPicker defaultValue
	let defaultValue = "#ff0000";
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
			defaultValue = `#${r}${g}${b}`;
			if (typeof parsed.a === "number") {
				const a = Math.round(parsed.a * 255)
					.toString(16)
					.padStart(2, "0");
				defaultValue += a;
			}
		}
	} catch {
		// Invalid JSON, use default
	}

	const handleColorChange = React.useCallback(
		(rgba: unknown) => {
			if (Array.isArray(rgba) && rgba.length >= 4) {
				const colorJson = JSON.stringify({
					r: Math.round(Number(rgba[0])),
					g: Math.round(Number(rgba[1])),
					b: Math.round(Number(rgba[2])),
					a: Number(rgba[3]),
				});
				setParam(id, "color", colorJson);
			}
		},
		[id, setParam],
	);

	const controls = (
		<div className="">
			<ColorPicker
				defaultValue={defaultValue}
				onChange={handleColorChange}
				className="max-w-md p-3"
			>
				<div className="flex flex-col gap-2">
					<ColorPickerSelection className="h-36 w-48 rounded" />
					<div className="flex gap-2">
						<ColorPickerHue className="flex-1" />
					</div>
					<ColorPickerAlpha />
				</div>
			</ColorPicker>
		</div>
	);

	return <BaseNode {...props} data={{ ...data, paramControls: controls }} />;
}
