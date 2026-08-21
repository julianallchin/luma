import * as React from "react";
import type { NodeProps } from "reactflow";
import { useGraphStore } from "@/features/patterns/stores/use-graph-store";
import { Input } from "@/shared/components/ui/input";
import { Selector } from "@/shared/components/ui/selector";
import { paramOptions } from "@/shared/lib/param-options";
import { BaseNode } from "./base-node";
import type { BaseNodeData } from "./types";

// Standard node with parameter controls
export const StandardNode = React.memo(function StandardNode(
	props: NodeProps<BaseNodeData>,
) {
	const { data, id } = props;
	const params = useGraphStore(
		(state) => state.nodeParams[id] ?? ({} as Record<string, unknown>),
	);
	const setParam = useGraphStore((state) => state.setParam);
	const [numberDrafts, setNumberDrafts] = React.useState<
		Record<string, string>
	>({});

	const controls: React.ReactNode[] = [];
	for (const param of data.definition.params) {
		const options = paramOptions(param);
		if (options) {
			const value =
				(params[param.id] as string) ?? param.defaultText ?? options[0].id;
			controls.push(
				<div key={param.id} className="px-2 pb-1">
					<span className="block text-[10px] text-gray-400 mb-1">
						{param.name}
					</span>
					<Selector
						value={value}
						onChange={(next) => setParam(id, param.id, next)}
						align="start"
						options={options.map((option) => ({
							value: option.id,
							label: option.label,
						}))}
					/>
				</div>,
			);
		} else if (param.paramType === "Number") {
			const draft = numberDrafts[param.id];
			const rawValue = params[param.id];
			const fallback = param.defaultNumber ?? 0;
			const value =
				draft ??
				(typeof rawValue === "number" ? rawValue.toString() : `${fallback}`);

			controls.push(
				<div key={param.id} className="px-2 pb-1">
					<label
						htmlFor={`${id}-${param.id}`}
						className="block text-[10px] text-gray-400 mb-1"
					>
						{param.name}
					</label>
					<Input
						id={`${id}-${param.id}`}
						type="number"
						value={value}
						onChange={(e) => {
							const text = e.target.value;
							setNumberDrafts((prev) => ({ ...prev, [param.id]: text }));
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
				</div>,
			);
		} else if (param.paramType === "Text") {
			const value = (params[param.id] as string) ?? param.defaultText ?? "";

			controls.push(
				<div key={param.id} className="px-2 pb-1">
					<label
						htmlFor={`${id}-${param.id}`}
						className="block text-[10px] text-gray-400 mb-1"
					>
						{param.name}
					</label>
					<Input
						id={`${id}-${param.id}`}
						type="text"
						value={value ?? ""}
						onChange={(e) => {
							setParam(id, param.id, e.target.value);
						}}
						className="h-7 text-xs"
					/>
				</div>,
			);
		}
	}

	const paramControls =
		controls.length > 0 ? <div className="py-1">{controls}</div> : null;

	return <BaseNode {...props} data={{ ...data, paramControls }} />;
});
