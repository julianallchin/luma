import { useMemo, useState } from "react";
import { cn } from "@/shared/lib/utils";
import {
	type CatalogPiece,
	listStageMeshes,
	type PaletteGroup,
} from "../lib/stage-meshes";
import { useStagePieceStore } from "../stores/use-stage-piece-store";

type IconProps = { className?: string };

function StageIcon({ className }: IconProps) {
	return (
		<svg
			viewBox="0 0 24 24"
			className={className}
			fill="none"
			stroke="currentColor"
			strokeWidth={2}
			strokeLinecap="round"
			strokeLinejoin="round"
			aria-hidden="true"
		>
			<rect x="3" y="10" width="18" height="5" />
			<line x1="6" y1="15" x2="6" y2="20" />
			<line x1="18" y1="15" x2="18" y2="20" />
		</svg>
	);
}

function TrussIcon({ className }: IconProps) {
	return (
		<svg
			viewBox="0 0 24 24"
			className={className}
			fill="none"
			stroke="currentColor"
			strokeWidth={2}
			strokeLinecap="round"
			strokeLinejoin="round"
			aria-hidden="true"
		>
			<rect x="2" y="8" width="20" height="8" />
			<line x1="2" y1="8" x2="22" y2="16" />
			<line x1="2" y1="16" x2="22" y2="8" />
		</svg>
	);
}

function SpeakerIcon({ className }: IconProps) {
	return (
		<svg
			viewBox="0 0 24 24"
			className={className}
			fill="none"
			stroke="currentColor"
			strokeWidth={2}
			strokeLinecap="round"
			strokeLinejoin="round"
			aria-hidden="true"
		>
			<path d="M7 3 L17 3 L19 21 L5 21 Z" />
			<circle cx="12" cy="15" r="3" />
			<circle cx="12" cy="8" r="1.25" />
		</svg>
	);
}

function EquipmentIcon({ className }: IconProps) {
	return (
		<svg
			viewBox="0 0 24 24"
			className={className}
			fill="none"
			stroke="currentColor"
			strokeWidth={2}
			strokeLinecap="round"
			strokeLinejoin="round"
			aria-hidden="true"
		>
			<rect x="3" y="6" width="18" height="12" />
			<circle cx="8" cy="12" r="2.25" />
			<line x1="15" y1="9" x2="15" y2="15" />
			<line x1="19" y1="9" x2="19" y2="15" />
		</svg>
	);
}

function AccessoriesIcon({ className }: IconProps) {
	return (
		<svg
			viewBox="0 0 24 24"
			className={className}
			fill="none"
			stroke="currentColor"
			strokeWidth={2}
			strokeLinecap="round"
			strokeLinejoin="round"
			aria-hidden="true"
		>
			<rect x="3" y="3" width="7" height="7" />
			<rect x="14" y="3" width="7" height="7" />
			<rect x="3" y="14" width="7" height="7" />
			<rect x="14" y="14" width="7" height="7" />
		</svg>
	);
}

interface SectionDef {
	group: PaletteGroup;
	label: string;
	Icon: React.ComponentType<IconProps>;
}

const SECTIONS: SectionDef[] = [
	{ group: "Stage", label: "Stage", Icon: StageIcon },
	{ group: "Trusses", label: "Truss", Icon: TrussIcon },
	{ group: "Speakers", label: "Speakers", Icon: SpeakerIcon },
	{ group: "Equipment", label: "Equipment", Icon: EquipmentIcon },
	{ group: "Accessories", label: "Accessories", Icon: AccessoriesIcon },
];

export function PropsOverlay() {
	const armedMeshPath = useStagePieceStore((s) => s.armedMeshPath);
	const armPlace = useStagePieceStore((s) => s.armPlace);
	const cancelPlace = useStagePieceStore((s) => s.cancelPlace);

	const [openGroup, setOpenGroup] = useState<PaletteGroup | null>(null);

	const meshesByGroup = useMemo(() => {
		const map = new Map<PaletteGroup, CatalogPiece[]>();
		for (const m of listStageMeshes()) {
			const bucket = map.get(m.paletteGroup) ?? [];
			bucket.push(m);
			map.set(m.paletteGroup, bucket);
		}
		return map;
	}, []);

	const currentList = openGroup ? (meshesByGroup.get(openGroup) ?? []) : [];

	return (
		<div className="absolute top-4 left-4 z-10 pointer-events-auto flex bg-gutter border border-trim select-none">
			<div className="flex flex-col">
				<div className="h-6 px-2 flex items-center bg-trim text-[9px] uppercase tracking-wider font-bold text-foreground/70">
					Props
				</div>
				{SECTIONS.map(({ group, label, Icon }, i) => {
					const isOpen = openGroup === group;
					return (
						<button
							key={group}
							type="button"
							onClick={() => setOpenGroup(isOpen ? null : group)}
							className={cn(
								"h-7 pl-2 pr-3 flex items-center gap-2 text-[9px] uppercase tracking-wider font-bold text-foreground/70 hover:bg-hover hover:text-foreground transition-colors",
								i > 0 && "border-t border-trim",
								isOpen && "bg-hover text-foreground",
							)}
						>
							<Icon className="h-3.5 w-3.5 shrink-0" />
							<span>{label}</span>
						</button>
					);
				})}
			</div>

			{openGroup && (
				<div className="flex flex-col border-l border-trim min-w-[180px]">
					<div className="h-6 px-2 flex items-center justify-between bg-trim text-[9px] uppercase tracking-wider font-bold text-foreground/70">
						<span>{openGroup}</span>
						{armedMeshPath && (
							<span className="text-foreground/50 normal-case tracking-normal font-normal">
								click · esc
							</span>
						)}
					</div>
					{currentList.length === 0 && (
						<div className="px-2 py-2 text-[10px] text-foreground/40 italic">
							Empty
						</div>
					)}
					{currentList.map((m, i) => {
						const isArmed = armedMeshPath === m.id;
						return (
							<button
								key={m.id}
								type="button"
								onClick={() => (isArmed ? cancelPlace() : armPlace(m.id))}
								className={cn(
									"h-7 px-2 flex items-center text-left text-[10px] text-foreground/80 hover:bg-hover hover:text-foreground transition-colors",
									i > 0 && "border-t border-trim",
									isArmed &&
										"bg-hover text-foreground ring-1 ring-inset ring-foreground/40",
								)}
							>
								<span className="truncate">{m.displayName}</span>
							</button>
						);
					})}
				</div>
			)}
		</div>
	);
}
