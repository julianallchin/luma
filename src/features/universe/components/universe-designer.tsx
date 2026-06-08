import { invoke } from "@tauri-apps/api/core";
import { AlertTriangle, Plus, Wand2 } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { useAppViewStore } from "@/features/app/stores/use-app-view-store";
import { StageHierarchy } from "@/features/stage/components/stage-hierarchy";
import { useStagePieceStore } from "@/features/stage/stores/use-stage-piece-store";
import { dmxStore } from "@/features/visualizer/stores/dmx-store";
import { universeStore } from "@/features/visualizer/stores/universe-state-store";
import { Button } from "@/shared/components/ui/button";
import { useFixtureStore } from "../stores/use-fixture-store";
import { AddFixtureDialog } from "./add-fixture-dialog";
import { DmxFootprint } from "./dmx-footprint";
import { GroupedFixtureTree } from "./grouped-fixture-tree";
import { PatchTable } from "./patch-table";
import { SimulationPane } from "./simulation-pane";

const PANEL_HEIGHT_KEY = "luma:universe-patch-panel-height";
const PANEL_MIN = 180;
const PANEL_MAX = 720;
const PANEL_DEFAULT = 360;

function readPanelHeight(): number {
	try {
		const raw = localStorage.getItem(PANEL_HEIGHT_KEY);
		if (!raw) return PANEL_DEFAULT;
		const n = Number(raw);
		if (!Number.isFinite(n)) return PANEL_DEFAULT;
		return Math.max(PANEL_MIN, Math.min(PANEL_MAX, n));
	} catch {
		return PANEL_DEFAULT;
	}
}

function writePanelHeight(height: number) {
	try {
		localStorage.setItem(PANEL_HEIGHT_KEY, String(height));
	} catch {
		// ignore
	}
}

interface UniverseDesignerProps {
	venueId?: string;
}

export function UniverseDesigner({ venueId }: UniverseDesignerProps) {
	const initialize = useFixtureStore((state) => state.initialize);
	const selectedPatchedIds = useFixtureStore(
		(state) => state.selectedPatchedIds,
	);
	const ungroupedCount = useFixtureStore(
		(state) => state.ungroupedFixtures.length,
	);
	const isReadOnly = useAppViewStore((s) => s.currentVenue?.role) === "member";

	const [addOpen, setAddOpen] = useState(false);
	const [panelHeight, setPanelHeight] = useState<number>(readPanelHeight);
	const panelRef = useRef<HTMLDivElement>(null);

	const initializeStagePieces = useStagePieceStore((s) => s.initialize);
	useEffect(() => {
		if (venueId) initializeStagePieces(venueId);
	}, [venueId, initializeStagePieces]);

	// Clear render engine + frontend caches so fixtures show as off
	useEffect(() => {
		invoke("render_clear_active_layer").catch(() => {});
		universeStore.clear();
		dmxStore.clear();
	}, []);

	// Blink-identify on selection changes.
	// - Pure add (cmd-click, shift-extend): flash only newly-added fixtures.
	// - Add + remove (replace-style: clicking a different group / single fixture):
	//   flash everything in the new selection, including overlap with the previous.
	// - Pure remove or no-op: no flash.
	const mountedRef = useRef(false);
	const prevSelectedRef = useRef<Set<string>>(new Set());
	useEffect(() => {
		if (!mountedRef.current) {
			mountedRef.current = true;
			prevSelectedRef.current = new Set(selectedPatchedIds);
			return;
		}
		const prev = prevSelectedRef.current;
		let addedCount = 0;
		let removedCount = 0;
		for (const id of selectedPatchedIds) if (!prev.has(id)) addedCount++;
		for (const id of prev) if (!selectedPatchedIds.has(id)) removedCount++;

		let toFlash: string[] = [];
		if (addedCount > 0) {
			toFlash =
				removedCount > 0
					? [...selectedPatchedIds]
					: [...selectedPatchedIds].filter((id) => !prev.has(id));
		}
		if (toFlash.length > 0) {
			invoke("render_identify_fixtures", { fixtureIds: toFlash }).catch(
				() => {},
			);
		}
		prevSelectedRef.current = new Set(selectedPatchedIds);
	}, [selectedPatchedIds]);

	useEffect(() => {
		if (venueId !== undefined) {
			initialize(venueId);
		}
	}, [initialize, venueId]);

	const handleResizeStart = useCallback(
		(e: React.MouseEvent) => {
			// Don't hijack clicks on the buttons inside the header bar
			if ((e.target as HTMLElement).closest("button")) return;
			e.preventDefault();
			const startY = e.clientY;
			const startHeight = panelHeight;
			const panel = panelRef.current;

			const handleMove = (ev: MouseEvent) => {
				const delta = startY - ev.clientY;
				const next = Math.max(
					PANEL_MIN,
					Math.min(PANEL_MAX, startHeight + delta),
				);
				if (panel) panel.style.height = `${next}px`;
				window.dispatchEvent(new Event("resize"));
			};

			const handleUp = (ev: MouseEvent) => {
				const delta = startY - ev.clientY;
				const final = Math.max(
					PANEL_MIN,
					Math.min(PANEL_MAX, startHeight + delta),
				);
				setPanelHeight(final);
				writePanelHeight(final);
				window.removeEventListener("mousemove", handleMove);
				window.removeEventListener("mouseup", handleUp);
			};

			window.addEventListener("mousemove", handleMove);
			window.addEventListener("mouseup", handleUp);
		},
		[panelHeight],
	);

	if (isReadOnly) {
		return (
			<div className="flex h-full w-full bg-background text-foreground overflow-hidden">
				<div className="flex-1 flex flex-col h-full min-w-0">
					<div className="flex-1 min-h-0 relative">
						<SimulationPane readOnly />
					</div>
					<div
						ref={panelRef}
						className="shrink-0 flex min-h-0"
						style={{ height: panelHeight }}
					>
						<div className="shrink-0 border-r border-trim">
							<DmxFootprint />
						</div>
						<div className="flex-1 min-w-0">
							<PatchTable />
						</div>
					</div>
				</div>
				<div className="w-80 border-l border-trim flex flex-col h-full">
					<GroupedFixtureTree />
				</div>
			</div>
		);
	}

	return (
		<div className="flex flex-col h-full w-full bg-background text-foreground overflow-hidden">
			{ungroupedCount > 0 && (
				<div className="flex items-center gap-2 px-3 py-1.5 bg-yellow-500/10 border-b border-yellow-500/30 text-yellow-200 text-xs shrink-0">
					<AlertTriangle className="h-3.5 w-3.5 text-yellow-400 shrink-0" />
					<span>
						{ungroupedCount} fixture{ungroupedCount !== 1 ? "s" : ""} not
						assigned to a group. Drag them into a group before leaving this
						page.
					</span>
				</div>
			)}

			{/* Top row: scene hierarchy | simulation (with inset Props overlay) | groups */}
			<div className="flex flex-1 min-h-0">
				<div className="w-64 border-r border-trim flex flex-col h-full shrink-0">
					<StageHierarchy />
				</div>

				<div className="flex-1 min-h-0 relative">
					<SimulationPane />
				</div>

				<div className="w-80 border-l border-trim flex flex-col h-full">
					<GroupedFixtureTree />
				</div>
			</div>

			{/* Bottom row: patch panel spanning full width */}
			<div
				ref={panelRef}
				className="shrink-0 flex flex-col min-h-0 border-t border-trim"
				style={{ height: panelHeight }}
			>
				{/* Header bar — also the resize handle */}
				{/* biome-ignore lint/a11y/noStaticElementInteractions: resize handle is mouse-only */}
				<div
					className="h-8 px-2 flex items-center gap-2 bg-trim shrink-0 cursor-row-resize select-none"
					onMouseDown={handleResizeStart}
				>
					<Button onClick={() => setAddOpen(true)}>
						<Plus />
						Add
					</Button>
					<Button disabled title="Auto Patch — coming soon">
						<Wand2 />
						Auto Patch
					</Button>
				</div>

				{/* Footprint + table */}
				<div className="flex flex-1 min-h-0">
					<div className="shrink-0 border-r border-trim">
						<DmxFootprint />
					</div>
					<div className="flex-1 min-w-0">
						<PatchTable />
					</div>
				</div>
			</div>

			<AddFixtureDialog open={addOpen} onOpenChange={setAddOpen} />
		</div>
	);
}
