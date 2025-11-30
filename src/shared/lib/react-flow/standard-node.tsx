import { List } from "lucide-react";
import * as React from "react";
import type { NodeProps } from "reactflow";
import { useGraphStore } from "@/features/patterns/stores/use-graph-store";
import { FixtureTree } from "@/features/universe/components/fixture-tree";
import { Button } from "@/shared/components/ui/button";
import { Input } from "@/shared/components/ui/input";
import {
	Popover,
	PopoverContent,
	PopoverTrigger,
} from "@/shared/components/ui/popover";
import { BaseNode } from "./base-node";
import type { BaseNodeData } from "./types";

// Standard node with parameter controls
export function StandardNode(props: NodeProps<BaseNodeData>) {
	const { data, id } = props;
	const params = useGraphStore(
		(state) => state.nodeParams[id] ?? ({} as Record<string, unknown>),
	);
	const setParam = useGraphStore((state) => state.setParam);
	const [numberDrafts, setNumberDrafts] = React.useState<Record<string, string>>(
		{},
	);

	const controls: React.ReactNode[] = [];
	for (const param of data.definition.params) {
		if (param.paramType === "Number") {
			const draft = numberDrafts[param.id];
			const rawValue = params[param.id];
			const fallback = param.defaultNumber ?? 0;
			const value =
				draft ?? (typeof rawValue === "number" ? rawValue.toString() : `${fallback}`);

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

			// Special handling for Selection Node
			if (data.typeId === "select" && param.id === "selected_ids") {
				let selectedIds: string[] = [];
				try {
					selectedIds = JSON.parse(value);
					if (!Array.isArray(selectedIds)) selectedIds = [];
				} catch {
					selectedIds = [];
				}

				controls.push(
					<div key={param.id} className="px-3 pb-1">
						<label className="block text-[10px] text-gray-400 mb-1">
							{param.name}
						</label>
						<Popover>
							<PopoverTrigger asChild>
								<Button
									variant="outline"
									size="sm"
									className="w-full justify-start text-left font-normal h-7 text-xs px-2"
								>
									<List className="mr-2 h-3 w-3" />
									{selectedIds.length > 0
										? `${selectedIds.length} selected`
										: "Select fixtures..."}
								</Button>
							</PopoverTrigger>
							<PopoverContent className="w-80 p-0 h-96" align="start">
								<FixtureTree
									selectedIds={selectedIds}
									onSelectionChange={(ids) => {
										setParam(id, param.id, JSON.stringify(ids));
									}}
								/>
							</PopoverContent>
						</Popover>
					</div>,
				);
			} else {
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
	}

	const paramControls =
		controls.length > 0 ? <div className="py-1">{controls}</div> : null;

	return <BaseNode {...props} data={{ ...data, paramControls }} />;
}
