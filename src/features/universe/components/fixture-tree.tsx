import { invoke } from "@tauri-apps/api/core";
import { Box, ChevronDown, ChevronRight, Disc } from "lucide-react";
import { useEffect, useState } from "react";
import { cn } from "@/shared/lib/utils";
import type { FixtureNode } from "../../../bindings/fixtures";

interface FixtureTreeProps {
	selectedIds: string[];
	onSelectionChange: (ids: string[]) => void;
}

export function FixtureTree({
	selectedIds,
	onSelectionChange,
}: FixtureTreeProps) {
	const [nodes, setNodes] = useState<FixtureNode[]>([]);
	const [expanded, setExpanded] = useState<Set<string>>(new Set());

	useEffect(() => {
		invoke<FixtureNode[]>("get_patch_hierarchy")
			.then(setNodes)
			.catch(console.error);
	}, []);

	const toggleExpand = (id: string) => {
		const next = new Set(expanded);
		if (next.has(id)) {
			next.delete(id);
		} else {
			next.add(id);
		}
		setExpanded(next);
	};

	const toggleSelection = (id: string, children: FixtureNode[]) => {
		const next = new Set(selectedIds);

		// Logic:
		// If fixture is selected, it implies all heads.
		// If specific head is selected, only that head.

		// For multiselect behavior:
		if (next.has(id)) {
			next.delete(id);
			// If unselecting a parent, should we unselect children?
			children.forEach((c) => {
				next.delete(c.id);
			});
		} else {
			next.add(id);
			// If selecting a parent, maybe select all children?
			// Or keep it as just parent ID and backend expands?
			// Backend expands "ParentID" -> All Heads.
			// Backend handles "ParentID:HeadIdx" -> Single Head.
			// So selecting Parent is enough.

			// However, for UI feedback, if we expand parent, should children be checked?
			// Let's keep it simple: Selected IDs list is exact.
		}

		onSelectionChange(Array.from(next));
	};

	const isSelected = (id: string) => selectedIds.includes(id);

	const renderNode = (node: FixtureNode, level: number) => {
		const isExpanded = expanded.has(node.id);
		const hasChildren = node.children.length > 0;
		const selected = isSelected(node.id);

		return (
			<div key={node.id}>
				<button
					type="button"
					className={cn(
						"flex items-center py-1 px-2 hover:bg-zinc-800 cursor-pointer text-sm w-full text-left",
						selected && "bg-blue-900/30 text-blue-200",
					)}
					style={{ paddingLeft: `${level * 12 + 8}px` }}
					onClick={() => toggleSelection(node.id, node.children)}
				>
					{/* biome-ignore lint/a11y/useSemanticElements: Cannot use button inside button */}
					<span
						className="p-1 hover:text-white text-zinc-500 cursor-pointer"
						onClick={(e) => {
							e.stopPropagation();
							toggleExpand(node.id);
						}}
						onKeyDown={(e) => {
							if (e.key === "Enter" || e.key === " ") {
								e.preventDefault();
								e.stopPropagation();
								toggleExpand(node.id);
							}
						}}
						role="button"
						tabIndex={0}
						aria-label={isExpanded ? "Collapse" : "Expand"}
					>
						{hasChildren ? (
							isExpanded ? (
								<ChevronDown size={14} />
							) : (
								<ChevronRight size={14} />
							)
						) : (
							<div className="w-[14px]" />
						)}
					</span>

					<div className="mr-2 text-zinc-400">
						{node.type === "fixture" ? <Box size={14} /> : <Disc size={14} />}
					</div>

					<span className="select-none">{node.label}</span>
				</button>

				{isExpanded &&
					node.children.map((child) => renderNode(child, level + 1))}
			</div>
		);
	};

	return (
		<div className="flex flex-col w-full h-full bg-zinc-900 border-r border-zinc-800 overflow-y-auto">
			<div className="p-2 text-xs font-bold text-zinc-500 uppercase tracking-wider">
				Patch Hierarchy
			</div>
			{nodes.map((node) => renderNode(node, 0))}
		</div>
	);
}
