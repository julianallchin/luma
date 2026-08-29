import { useMemo } from "react";
import type { ResolvedNode } from "@/bindings/venue-graph";
import { getStageMesh } from "../lib/stage-meshes";
import { useVenueStore } from "../stores/use-venue-store";

interface FlatRow {
	id: string;
	label: string;
	depth: number;
}

function labelOf(node: ResolvedNode): string {
	if (node.label) return node.label;
	const piece = node.catalogRef ? getStageMesh(node.catalogRef) : null;
	return piece?.displayName ?? node.catalogRef ?? node.kind;
}

/**
 * The venue's structure, as the solver returns it.
 *
 * Read-only: the venue graph is edited in the gpui builder, and this app has
 * no write verbs for it. Nodes come back depth-first from the root already, so
 * indentation is a walk up `parentId` rather than a rebuilt tree.
 */
export function StageHierarchy() {
	const nodes = useVenueStore((s) => s.nodes);
	const warnings = useVenueStore((s) => s.warnings);

	const rows = useMemo<FlatRow[]>(() => {
		const byId = new Map(nodes.map((n) => [n.id, n]));
		const depthOf = (node: ResolvedNode): number => {
			let depth = 0;
			let parent = node.parentId ? byId.get(node.parentId) : undefined;
			while (parent) {
				depth++;
				parent = parent.parentId ? byId.get(parent.parentId) : undefined;
			}
			// The venue root is the frame, not a piece, and is not listed.
			return Math.max(0, depth - 1);
		};
		return nodes
			.filter((n) => n.kind !== "venue" && n.kind !== "fixture")
			.map((n) => ({ id: n.id, label: labelOf(n), depth: depthOf(n) }));
	}, [nodes]);

	return (
		<div className="flex flex-col h-full min-h-0 overflow-y-auto">
			<div className="h-7 px-2 flex items-center justify-between gap-2 bg-trim text-[9px] uppercase tracking-wider font-bold text-foreground/70 sticky top-0 z-10">
				<span>Scene ({rows.length})</span>
				<span className="text-foreground/40">Read only</span>
			</div>
			<div className="px-2 py-1.5 text-[9px] uppercase tracking-wider font-bold text-foreground/50 bg-stripe border-b border-trim">
				Built in the stage builder
			</div>
			{rows.length === 0 && (
				<div className="px-2 py-3 text-[10px] text-foreground/40 italic">
					Empty stage.
				</div>
			)}
			<ul>
				{rows.map((row) => (
					<li key={row.id}>
						<div
							className="w-full h-7 pr-1 flex items-center text-[10px] text-foreground/80 border-b border-trim/40 last:border-b-0"
							style={{ paddingLeft: 8 + row.depth * 12 }}
						>
							{row.depth > 0 && (
								<span className="text-foreground/30 mr-1 select-none">└</span>
							)}
							<span className="truncate">{row.label}</span>
						</div>
					</li>
				))}
			</ul>
			{warnings.length > 0 && (
				<div className="mt-auto border-t border-trim">
					<div className="h-6 px-2 flex items-center bg-gutter text-[9px] uppercase tracking-wider font-bold text-foreground/50">
						Solver ({warnings.length})
					</div>
					{warnings.map((warning) => (
						<div
							key={warning}
							className="px-2 py-1 text-[10px] text-yellow-200/80 border-b border-trim/40 last:border-b-0"
						>
							{warning}
						</div>
					))}
				</div>
			)}
		</div>
	);
}
