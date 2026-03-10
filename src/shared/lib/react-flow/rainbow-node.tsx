import * as React from "react";
import type { NodeProps } from "reactflow";
import { useGraphStore } from "@/features/patterns/stores/use-graph-store";
import { Input } from "@/shared/components/ui/input";
import { BaseNode } from "./base-node";
import type { BaseNodeData } from "./types";

function hslToHex(h: number, s: number, l: number): string {
	const hue2rgb = (p: number, q: number, t: number) => {
		let tt = t;
		if (tt < 0) tt += 1;
		if (tt > 1) tt -= 1;
		if (tt < 1 / 6) return p + (q - p) * 6 * tt;
		if (tt < 1 / 2) return q;
		if (tt < 2 / 3) return p + (q - p) * (2 / 3 - tt) * 6;
		return p;
	};
	const q = l < 0.5 ? l * (1 + s) : l + s - l * s;
	const p = 2 * l - q;
	const r = Math.round(hue2rgb(p, q, h + 1 / 3) * 255);
	const g = Math.round(hue2rgb(p, q, h) * 255);
	const b = Math.round(hue2rgb(p, q, h - 1 / 3) * 255);
	return `#${r.toString(16).padStart(2, "0")}${g.toString(16).padStart(2, "0")}${b.toString(16).padStart(2, "0")}`;
}

export function RainbowNode(props: NodeProps<BaseNodeData>) {
	const { data, id } = props;
	const params = useGraphStore(
		(state) => state.nodeParams[id] ?? ({} as Record<string, unknown>),
	);
	const setParam = useGraphStore((state) => state.setParam);
	const [numberDrafts, setNumberDrafts] = React.useState<
		Record<string, string>
	>({});

	const offset =
		typeof params.offset === "number"
			? params.offset
			: (data.definition.params.find((p) => p.id === "offset")?.defaultNumber ??
				0);
	const spread =
		typeof params.spread === "number"
			? params.spread
			: (data.definition.params.find((p) => p.id === "spread")?.defaultNumber ??
				1);
	const saturation =
		typeof params.saturation === "number"
			? params.saturation
			: (data.definition.params.find((p) => p.id === "saturation")
					?.defaultNumber ?? 1);

	// Build gradient CSS from current params
	const gradientStops = React.useMemo(() => {
		const stops: string[] = [];
		const n = 12;
		for (let i = 0; i <= n; i++) {
			const v = i / n;
			let hue = (v * spread + offset) % 1;
			if (hue < 0) hue += 1;
			const hex = hslToHex(hue, Math.max(0, Math.min(1, saturation)), 0.5);
			stops.push(`${hex} ${((i / n) * 100).toFixed(1)}%`);
		}
		return stops.join(", ");
	}, [offset, spread, saturation]);

	const numberParams = data.definition.params.filter(
		(p) => p.paramType === "Number",
	);

	const controls = (
		<div className="p-2 space-y-2 nodrag">
			<div
				className="h-6 w-full rounded border border-border"
				style={{
					background: `linear-gradient(to right, ${gradientStops})`,
				}}
			/>
			{numberParams.map((param) => {
				const draft = numberDrafts[param.id];
				const rawValue = params[param.id];
				const fallback = param.defaultNumber ?? 0;
				const value =
					draft ??
					(typeof rawValue === "number" ? rawValue.toString() : `${fallback}`);

				return (
					<div key={param.id}>
						<label
							htmlFor={`${id}-${param.id}`}
							className="block text-[10px] text-gray-400 mb-0.5"
						>
							{param.name}
						</label>
						<Input
							id={`${id}-${param.id}`}
							type="number"
							step={param.id === "saturation" ? 0.1 : 0.05}
							value={value}
							onChange={(e) => {
								const text = e.target.value;
								setNumberDrafts((prev) => ({
									...prev,
									[param.id]: text,
								}));
								const next = Number(text);
								if (Number.isFinite(next)) {
									setParam(id, param.id, next);
								}
							}}
							onBlur={() => {
								setNumberDrafts((prev) => {
									const nextDrafts = { ...prev };
									delete nextDrafts[param.id];
									return nextDrafts;
								});
							}}
							className="h-7 text-xs"
						/>
					</div>
				);
			})}
		</div>
	);

	return <BaseNode {...props} data={{ ...data, paramControls: controls }} />;
}
