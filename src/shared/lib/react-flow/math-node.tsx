import * as React from "react";
import type { NodeProps } from "reactflow";
import { useGraphStore } from "@/features/patterns/stores/use-graph-store";
import { Input } from "@/shared/components/ui/input";
import {
	Select,
	SelectContent,
	SelectItem,
	SelectTrigger,
	SelectValue,
} from "@/shared/components/ui/select";
import { BaseNode } from "./base-node";
import type { BaseNodeData } from "./types";

const OPS = [
	{ id: "add", label: "Add" },
	{ id: "subtract", label: "Subtract" },
	{ id: "multiply", label: "Multiply" },
	{ id: "divide", label: "Divide" },
	{ id: "max", label: "Max" },
	{ id: "min", label: "Min" },
] as const;

export function MathNode(props: NodeProps<BaseNodeData>) {
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
		if (param.id === "operation") {
			const value = (params[param.id] as string) ?? param.defaultText ?? "add";
			controls.push(
				<div key={param.id} className="px-1">
					<Select
						value={value}
						onValueChange={(val) => setParam(id, param.id, val)}
					>
						<SelectTrigger className="h-8 text-xs w-full">
							<SelectValue placeholder="Select operation" />
						</SelectTrigger>
						<SelectContent className="text-xs">
							{OPS.map((op) => (
								<SelectItem key={op.id} value={op.id}>
									{op.label}
								</SelectItem>
							))}
						</SelectContent>
					</Select>
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
						value={value}
						onChange={(e) => setParam(id, param.id, e.target.value)}
						className="h-7 text-xs"
					/>
				</div>,
			);
		}
	}

	const paramControls =
		controls.length > 0 ? <div className="py-1">{controls}</div> : null;

	return (
		<div className="max-w-48">
			<BaseNode {...props} data={{ ...data, paramControls }} />
		</div>
	);
}
