import { useCallback, useEffect, useState } from "react";
import type { PatchedFixture } from "@/bindings/fixtures";
import { cn } from "@/shared/lib/utils";
import { useFixtureStore } from "../stores/use-fixture-store";

export function AssignmentMatrix() {
	const {
		patchedFixtures,
		patchFixture,
		movePatchedFixture,
		removePatchedFixture,
		selectedPatchedId,
		setSelectedPatchedId,
	} = useFixtureStore();
	const [draggingFixtureId, setDraggingFixtureId] = useState<string | null>(
		null,
	);
	const [manualMove, setManualMove] = useState<{
		fixtureId: string;
		numChannels: number;
		modeName: string;
	} | null>(null);
	const [pointerDown, setPointerDown] = useState<{
		fixtureId: string;
		numChannels: number;
		modeName: string;
		startX: number;
		startY: number;
	} | null>(null);
	const [hoverState, setHoverState] = useState<{
		address: number;
		numChannels: number;
		valid: boolean;
	} | null>(null);

	const parsePayload = (dt: DataTransfer) => {
		console.debug("[AssignmentMatrix] parsePayload types", dt.types);
		const rawJson =
			dt.getData("application/json") ||
			dt.getData("text/plain") ||
			(window as any).__lumaDragPayload;
		if (!rawJson) return null;
		try {
			return JSON.parse(rawJson);
		} catch (err) {
			console.error("Invalid drag payload", err);
			return null;
		}
	};

	const GAP_PX = 2; // grid gap is gap-0.5 (0.125rem) -> 2px
	const DRAG_THRESHOLD = 3; // px before treating a press as a move

	const validatePlacement = (
		address: number,
		numChannels: number,
		ignoreId?: string | null,
	) => {
		const endAddress = address + numChannels - 1;
		if (endAddress > 512) return { valid: false, endAddress };
		const hasOverlap = patchedFixtures.some((f) => {
			const fEnd = f.address + f.numChannels - 1;
			return f.id !== ignoreId && address <= fEnd && endAddress >= f.address;
		});
		return { valid: !hasOverlap, endAddress };
	};

	const handlePreview = (
		address: number,
		numChannels: number,
		ignoreId?: string | null,
	) => {
		if (numChannels <= 0) return;
		const { valid } = validatePlacement(address, numChannels, ignoreId);
		setHoverState({ address, numChannels, valid });
	};

	// Handle drag over to show preview
	const handleDragOver = (e: React.DragEvent, address: number) => {
		e.preventDefault();
		if (e.dataTransfer) {
			e.dataTransfer.dropEffect = "move";
		}
		try {
			const data = parsePayload(e.dataTransfer) ?? {};
			const payloadChannels = Number(data.numChannels ?? 0);
			const activeFixtureId = data.fixtureId || draggingFixtureId;
			const activeFixture = activeFixtureId
				? patchedFixtures.find((f) => f.id === activeFixtureId)
				: null;
			const numChannels =
				payloadChannels > 0
					? payloadChannels
					: Number(activeFixture?.numChannels ?? 0);
			if (numChannels > 0) {
				handlePreview(address, numChannels, activeFixtureId);
			}
		} catch (err) {
			// Data transfer might not be available during dragover in some browsers
		}
	};

	const handleDrop = async (e: React.DragEvent, address: number) => {
		e.preventDefault();
		setHoverState(null);

		try {
			const data = parsePayload(e.dataTransfer);
			console.debug("[AssignmentMatrix] drop event", { address, data });
			const payloadChannels = Number(data?.numChannels ?? 0);
			const activeFixtureId = data?.fixtureId || draggingFixtureId;
			const fromFixture = activeFixtureId
				? patchedFixtures.find((f) => f.id === activeFixtureId)
				: null;
			const numChannels =
				payloadChannels > 0
					? payloadChannels
					: Number(fromFixture?.numChannels ?? 0);
			const modeName = data?.modeName ?? fromFixture?.modeName;

			if (modeName && numChannels > 0) {
				console.debug("[AssignmentMatrix] drop", { address, data });
				await attemptPlace(address, {
					fixtureId: data.fixtureId,
					modeName,
					numChannels,
				});
			}
		} catch (err) {
			console.error("Drop failed", err);
		} finally {
			setDraggingFixtureId(null);
		}
	};

	const attemptPlace = useCallback(
		async (
			address: number,
			payload: { fixtureId?: string; modeName: string; numChannels: number },
		) => {
			const ignoreId = payload.fixtureId || draggingFixtureId;
			const { valid } = validatePlacement(
				address,
				payload.numChannels,
				ignoreId,
			);
			if (!valid) return;

			if (payload.fixtureId) {
				console.debug("[AssignmentMatrix] move fixture", {
					id: payload.fixtureId,
					to: address,
				});
				await movePatchedFixture(payload.fixtureId, address);
				setSelectedPatchedId(payload.fixtureId);
			} else {
				await patchFixture(1, address, payload.modeName, payload.numChannels);
			}
			setManualMove(null);
			setDraggingFixtureId(null);
			setHoverState(null);
		},
		[
			draggingFixtureId,
			movePatchedFixture,
			patchFixture,
			setSelectedPatchedId,
			validatePlacement,
		],
	);
	// Pointer-driven move (non-DOM drag) start when moving past a small threshold
	useEffect(() => {
		const handleMove = (e: MouseEvent) => {
			if (!pointerDown || manualMove) return;
			const dx = Math.abs(e.clientX - pointerDown.startX);
			const dy = Math.abs(e.clientY - pointerDown.startY);
			if (dx > DRAG_THRESHOLD || dy > DRAG_THRESHOLD) {
				setManualMove({
					fixtureId: pointerDown.fixtureId,
					numChannels: pointerDown.numChannels,
					modeName: pointerDown.modeName,
				});
				setDraggingFixtureId(pointerDown.fixtureId);
				setHoverState(null);
			}
		};

		const handleUp = () => {
			if (manualMove && hoverState) {
				attemptPlace(hoverState.address, {
					fixtureId: manualMove.fixtureId,
					modeName: manualMove.modeName,
					numChannels: manualMove.numChannels,
				});
			} else if (manualMove && !hoverState) {
				setManualMove(null);
				setDraggingFixtureId(null);
				setHoverState(null);
			}
			setPointerDown(null);
			if (!manualMove) {
				setDraggingFixtureId(null);
				setHoverState(null);
			}
		};

		window.addEventListener("mousemove", handleMove);
		window.addEventListener("mouseup", handleUp);
		return () => {
			window.removeEventListener("mousemove", handleMove);
			window.removeEventListener("mouseup", handleUp);
		};
	}, [manualMove, hoverState, attemptPlace, pointerDown]);

	const handleManualHover = (address: number) => {
		if (!manualMove) return;
		handlePreview(address, manualMove.numChannels, manualMove.fixtureId);
	};

	// Cancel manual move with Escape
	useEffect(() => {
		const onKey = (e: KeyboardEvent) => {
			if (e.key === "Escape" && manualMove) {
				setManualMove(null);
				setDraggingFixtureId(null);
				setHoverState(null);
				setPointerDown(null);
			}
		};
		window.addEventListener("keydown", onKey);
		return () => window.removeEventListener("keydown", onKey);
	}, [manualMove]);

	// Helper to render patched fixtures
	const renderCellContent = (i: number) => {
		const address = i + 1;

		// Check if this cell is the start of a patched fixture
		const fixture = patchedFixtures.find(
			(f) => f.address === address && f.id !== draggingFixtureId,
		);
		if (fixture) {
			const numChannels = Number(fixture.numChannels ?? 0);
			const label = fixture.label ?? fixture.model;
			return (
				<div
					style={{
						width: `calc(${numChannels} * (100% + ${GAP_PX}px) - ${GAP_PX}px)`,
						boxSizing: "border-box",
						zIndex: 20,
					}}
					title={`${fixture.manufacturer} ${fixture.model} (${fixture.modeName}) #${fixture.label ?? ""}`}
					onContextMenu={(e) => {
						e.preventDefault();
						if (confirm(`Unpatch ${fixture.model}?`)) {
							removePatchedFixture(fixture.id);
						}
					}}
					onMouseDown={(e) => {
						if (e.button !== 0) return;
						e.stopPropagation();
						setSelectedPatchedId(fixture.id);
						setPointerDown({
							fixtureId: fixture.id,
							numChannels: Number(fixture.numChannels ?? 0),
							modeName: fixture.modeName ?? "",
							startX: e.clientX,
							startY: e.clientY,
						});
						setHoverState(null);
					}}
					onClick={(e) => {
						e.stopPropagation();
						setSelectedPatchedId(fixture.id);
					}}
					aria-label={`Fixture ${label}`}
					data-selected={selectedPatchedId === fixture.id}
					className={cn(
						"absolute inset-0 z-10 bg-primary/20 border text-primary-foreground text-[10px] flex flex-col items-center justify-center overflow-hidden select-none",
						selectedPatchedId === fixture.id
							? "border-primary bg-primary/80"
							: "border-primary/80",
					)}
				>
					<span
						className={cn(
							"font-bold truncate w-full text-center px-1",
							selectedPatchedId === fixture.id
								? "text-primary-foreground"
								: "text-accent",
						)}
					>
						{label}
					</span>
				</div>
			);
		}

		// Check if occupied by a fixture (but not start)
		const isOccupied = patchedFixtures.some((f) => {
			const span = Number(f.numChannels ?? 0);
			return (
				f.id !== draggingFixtureId &&
				address > f.address &&
				address < f.address + span
			);
		});
		if (isOccupied) return null; // Covered by the main block

		return (
			<span className="text-[9px] text-muted-foreground/50 select-none">
				{address}
			</span>
		);
	};

	return (
		<div className="w-full h-full bg-background p-4 overflow-auto">
			<h3 className="text-xs font-semibold mb-2 text-muted-foreground">
				DMX Patch (Universe 1)
			</h3>
			<div className="grid grid-cols-[repeat(auto-fill,minmax(30px,1fr))] relative">
				{Array.from({ length: 512 }).map((_, i) => {
					const address = i + 1;

					// Check hover state
					let highlightClass = "";
					if (hoverState) {
						const endHover = hoverState.address + hoverState.numChannels - 1;
						if (address >= hoverState.address && address <= endHover) {
							highlightClass = hoverState.valid
								? "bg-green-500/30"
								: "bg-red-500/30";
						}
					}

					return (
						<div
							key={i}
							className={cn(
								"aspect-square border border-background bg-card flex items-center justify-center relative",
								highlightClass,
							)}
							onMouseEnter={() => handleManualHover(address)}
							onMouseLeave={() => setHoverState(null)}
							onDragOver={(e) => handleDragOver(e, address)}
							onDragLeave={() => setHoverState(null)}
							onDrop={(e) => handleDrop(e, address)}
						>
							{renderCellContent(i)}
						</div>
					);
				})}
			</div>
		</div>
	);
}
