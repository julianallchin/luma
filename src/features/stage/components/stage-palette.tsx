import { cn } from "@/shared/lib/utils";
import {
	type CatalogPiece,
	listStageMeshes,
	PALETTE_GROUP_ORDER,
} from "../lib/stage-meshes";
import { useStagePieceStore } from "../stores/use-stage-piece-store";

export function StagePalette() {
	const armedMeshPath = useStagePieceStore((s) => s.armedMeshPath);
	const armPlace = useStagePieceStore((s) => s.armPlace);
	const cancelPlace = useStagePieceStore((s) => s.cancelPlace);

	const meshes = listStageMeshes();
	const groups: Record<string, CatalogPiece[]> = {};
	for (const m of meshes) {
		const bucket = groups[m.paletteGroup] ?? [];
		bucket.push(m);
		groups[m.paletteGroup] = bucket;
	}

	return (
		<div className="flex flex-col h-full min-h-0 overflow-y-auto">
			<div className="h-7 px-2 flex items-center bg-trim text-[9px] uppercase tracking-wider font-bold text-foreground/70 sticky top-0 z-10">
				Stage Library
			</div>
			{armedMeshPath && (
				<div className="px-2 py-1.5 text-[9px] uppercase tracking-wider font-bold text-foreground/60 bg-stripe border-b border-trim">
					Click in scene to place · esc to cancel
				</div>
			)}
			{PALETTE_GROUP_ORDER.map((group) => {
				const items = groups[group];
				if (!items?.length) return null;
				return (
					<div key={group} className="border-b border-trim">
						<div className="h-6 px-2 flex items-center text-[9px] uppercase tracking-wider font-bold text-foreground/50 bg-gutter">
							{group}
						</div>
						<ul>
							{items.map((m) => {
								const isArmed = armedMeshPath === m.id;
								return (
									<li key={m.id}>
										<button
											type="button"
											onClick={() => (isArmed ? cancelPlace() : armPlace(m.id))}
											className={cn(
												"w-full text-left h-7 px-2 flex items-center justify-between gap-2",
												"text-[10px] text-foreground/80 border-b border-trim/40 last:border-b-0",
												"hover:bg-hover transition-colors",
												isArmed &&
													"bg-hover text-foreground ring-1 ring-inset ring-foreground/40",
											)}
										>
											<span className="truncate">{m.displayName}</span>
										</button>
									</li>
								);
							})}
						</ul>
					</div>
				);
			})}
		</div>
	);
}
