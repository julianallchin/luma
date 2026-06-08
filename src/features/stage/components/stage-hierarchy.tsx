import { Trash2 } from "lucide-react";
import { useMemo } from "react";
import { cn } from "@/shared/lib/utils";
import { getStageMesh } from "../lib/stage-meshes";
import { buildTree, type StagePieceNode } from "../lib/tree";
import { useStagePieceStore } from "../stores/use-stage-piece-store";

interface FlatRow {
	id: string;
	label: string;
	depth: number;
}

function flattenTree(roots: StagePieceNode[]): FlatRow[] {
	const out: FlatRow[] = [];
	const walk = (node: StagePieceNode, depth: number) => {
		const def = getStageMesh(node.piece.meshPath);
		out.push({
			id: node.piece.id,
			label: node.piece.label ?? def?.displayName ?? node.piece.meshPath,
			depth,
		});
		for (const child of node.children) walk(child, depth + 1);
	};
	for (const root of roots) walk(root, 0);
	return out;
}

export function StageHierarchy() {
	const pieces = useStagePieceStore((s) => s.pieces);
	const selectedId = useStagePieceStore((s) => s.selectedId);
	const hoveredId = useStagePieceStore((s) => s.hoveredId);
	const selectPiece = useStagePieceStore((s) => s.selectPiece);
	const setHoveredId = useStagePieceStore((s) => s.setHoveredId);
	const deletePiece = useStagePieceStore((s) => s.deletePiece);

	const rows = useMemo(() => flattenTree(buildTree(pieces)), [pieces]);

	return (
		// biome-ignore lint/a11y/noStaticElementInteractions: hover preview for 3D scene; not a primary interaction surface
		<div
			className="flex flex-col h-full min-h-0 overflow-y-auto"
			onMouseLeave={() => setHoveredId(null)}
		>
			<div className="h-7 px-2 flex items-center bg-trim text-[9px] uppercase tracking-wider font-bold text-foreground/70 sticky top-0 z-10">
				Scene ({pieces.length})
			</div>
			{pieces.length === 0 && (
				<div className="px-2 py-3 text-[10px] text-foreground/40 italic">
					Empty stage. Add pieces from the library above.
				</div>
			)}
			<ul>
				{rows.map((row) => {
					const isSelected = selectedId === row.id;
					const isHovered = hoveredId === row.id;
					return (
						<li key={row.id}>
							{/* biome-ignore lint/a11y/noStaticElementInteractions: hover preview for 3D scene; selection happens via the inner button */}
							<div
								className={cn(
									"group w-full h-7 pr-1 flex items-center justify-between gap-2",
									"text-[10px] text-foreground/80 border-b border-trim/40 last:border-b-0",
									"hover:bg-hover transition-colors",
									isSelected &&
										"bg-hover text-foreground ring-1 ring-inset ring-foreground/40",
									!isSelected && isHovered && "bg-hover/60",
								)}
								onMouseEnter={() => setHoveredId(row.id)}
							>
								<button
									type="button"
									onClick={() => selectPiece(isSelected ? null : row.id)}
									className="flex-1 min-w-0 h-full text-left truncate flex items-center"
									style={{ paddingLeft: 8 + row.depth * 12 }}
								>
									{row.depth > 0 && (
										<span className="text-foreground/30 mr-1 select-none">
											└
										</span>
									)}
									<span className="truncate">{row.label}</span>
								</button>
								<button
									type="button"
									aria-label="Delete piece"
									className="opacity-0 group-hover:opacity-100 hover:text-red-400 transition-opacity shrink-0"
									onClick={(e) => {
										e.stopPropagation();
										deletePiece(row.id);
									}}
								>
									<Trash2 className="h-3 w-3" />
								</button>
							</div>
						</li>
					);
				})}
			</ul>
		</div>
	);
}
