import type * as React from "react";
import type { NodeProps } from "reactflow";
import { useGraphStore } from "@/features/patterns/stores/use-graph-store";
import { Input } from "@/shared/components/ui/input";
import { BaseNode } from "./base-node";
import type { BaseNodeData } from "./types";

// Standard node with parameter controls
export function StandardNode(props: NodeProps<BaseNodeData>) {
	const { data, id } = props;
	const params = useGraphStore(
		(state) => state.nodeParams[id] ?? ({} as Record<string, unknown>),
	);
	const setParam = useGraphStore((state) => state.setParam);

	const controls: React.ReactNode[] = [];
	for (const param of data.definition.params) {
		if (param.paramType === "Number") {
			const value = (params[param.id] as number) ?? param.defaultNumber ?? 0;
			controls.push(
				<div key={param.id} className="px-3 pb-1">
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
							const next = Number(e.target.value);
							setParam(id, param.id, Number.isFinite(next) ? next : 0);
						}}
						className="h-7 text-xs"
					/>
				</div>,
			);
		} else if (param.paramType === "Text") {
			const value = (params[param.id] as string) ?? param.defaultText ?? "";
			controls.push(
				<div key={param.id} className="px-3 pb-1">
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
}
