import type React from "react";
import { useCallback, useEffect, useMemo, useState } from "react";
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
	const fixtureParity = useMemo(() => {
		const ordered = [...patchedFixtures].sort((a, b) => a.address - b.address);
		return new Map(ordered.map((f, idx) => [f.id, idx % 2 === 1]));
	}, [patchedFixtures]);

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

	const DRAG_THRESHOLD = 3; // px before treating a press as a move

	const getFixtureAtAddress = useCallback(
		(address: number) =>
			patchedFixtures.find((f) => {
				const span = Number(f.numChannels ?? 0);
				return (
					f.id !== draggingFixtureId &&
					address >= f.address &&
					address < f.address + span
				);
			}) ?? null,
		[draggingFixtureId, patchedFixtures],
	);

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

	const handleFixtureContextMenu = (
		e: React.MouseEvent,
		fixture: PatchedFixture,
	) => {
		e.preventDefault();
		if (confirm(`Unpatch ${fixture.model}?`)) {
			removePatchedFixture(fixture.id);
		}
	};

	const handleFixtureMouseDown = (
		e: React.MouseEvent,
		fixture: PatchedFixture,
	) => {
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
	};

	const handleFixtureClick = (e: React.MouseEvent, fixture: PatchedFixture) => {
		e.stopPropagation();
		setSelectedPatchedId(fixture.id);
	};

	return (
		<div className="w-full h-full bg-background p-4 overflow-auto">
			<h3 className="text-xs font-semibold mb-2 text-muted-foreground uppercase">
				Universe 1 map
			</h3>
			<div className="grid grid-cols-[repeat(auto-fill,minmax(40px,1fr))] relative">
				{Array.from({ length: 512 }).map((_, i) => {
					const address = i + 1;
					const fixture = getFixtureAtAddress(address);
					const isStartCell = fixture?.address === address;
					const isSelected =
						fixture && selectedPatchedId === fixture.id && !draggingFixtureId;
					const isOdd = fixture
						? fixtureParity.get(fixture.id) === true
						: false;
					const inPreview =
						hoverState &&
						address >= hoverState.address &&
						address <= hoverState.address + hoverState.numChannels - 1;
					const label = fixture?.label ?? fixture?.model;
					const cellColor = isOdd ? "#c1723f" : "#a1474f";

					let background = "";
					let opacity = 1;
					if (fixture) {
						background = isSelected ? "#4e99ac" : cellColor;
						// opacity = isStartCell ? 1 : 0.4;
					} else if (inPreview) {
						background = hoverState?.valid ? "#22c55e" : "#ef4444";
						opacity = 0.5;
					}

					const cellClasses = cn(
						"aspect-square border border-background flex items-center justify-center relative cursor-default overflow-visible",
						!fixture &&
							!inPreview &&
							"bg-card hover:bg-input text-muted-foreground/60",
					);

					return (
						<div
							key={i}
							className={cellClasses}
							style={{
								background: background || undefined,
								opacity,
							}}
							onMouseEnter={() => handleManualHover(address)}
							onMouseLeave={() => setHoverState(null)}
							onDragOver={(e) => handleDragOver(e, address)}
							onDragLeave={() => setHoverState(null)}
							onDrop={(e) => handleDrop(e, address)}
							onContextMenu={
								fixture
									? (e) => handleFixtureContextMenu(e, fixture)
									: undefined
							}
							onMouseDown={(e) =>
								fixture
									? handleFixtureMouseDown(e, fixture)
									: setSelectedPatchedId(null)
							}
							onClick={
								fixture ? (e) => handleFixtureClick(e, fixture) : undefined
							}
							aria-label={fixture ? `Fixture ${label}` : `Address ${address}`}
						>
							{fixture && isStartCell && (
								<span
									className={cn(
										"absolute left-0 top-0 -mt-1.5 pl-1 text-[10px] whitespace-nowrap pointer-events-none font-semibold tracking-tighter font-mono",
										"text-white z-10",
									)}
									title={`${fixture.manufacturer} ${fixture.model} (${fixture.modeName}) #${fixture.label ?? ""}`}
								>
									{label}
								</span>
							)}
							<span
								className={cn(
									"text-[9px] select-none font-mono font-semibold",
									fixture ? "text-black" : "text-muted-foreground/50",
								)}
							>
								{address}
							</span>
						</div>
					);
				})}
			</div>
		</div>
	);
}
