import { useMemo } from "react";
import type { PatchedFixture } from "@/bindings/fixtures";
import { cn } from "@/shared/lib/utils";
import { useFixtureStore } from "../stores/use-fixture-store";

const ROWS = 16;
const COLS = 32;
const TOTAL = ROWS * COLS;

export function DmxFootprint() {
	const patchedFixtures = useFixtureStore((s) => s.patchedFixtures);
	const selectedPatchedIds = useFixtureStore((s) => s.selectedPatchedIds);
	const selectFixtureById = useFixtureStore((s) => s.selectFixtureById);

	const addressToFixture = useMemo(() => {
		const map = new Map<number, PatchedFixture>();
		for (const f of patchedFixtures) {
			const start = Number(f.address);
			const span = Number(f.numChannels ?? 0);
			for (let addr = start; addr < start + span; addr++) {
				map.set(addr, f);
			}
		}
		return map;
	}, [patchedFixtures]);

	return (
		<div className="flex flex-col h-full bg-card border-r border-trim">
			<div className="px-2 py-1 text-[9px] uppercase tracking-wider font-bold text-foreground/70 border-b border-trim">
				Universe 1
			</div>
			<div className="p-2 flex-1 overflow-auto">
				<div
					className="grid gap-[1px] w-max"
					style={{
						gridTemplateColumns: `repeat(${COLS}, 10px)`,
						gridAutoRows: "10px",
					}}
				>
					{Array.from({ length: TOTAL }).map((_, i) => {
						const address = i + 1;
						const fixture = addressToFixture.get(address);
						const isSelected = fixture && selectedPatchedIds.has(fixture.id);

						let background = "rgb(46 46 46)";
						if (isSelected) {
							background = "var(--primary)";
						} else if (fixture) {
							background = "rgb(95 95 95)";
						}

						return (
							<button
								key={`fp-${address}`}
								type="button"
								title={
									fixture
										? `${fixture.label ?? fixture.model} @ ${fixture.address}`
										: `Free (ch ${address})`
								}
								className={cn(
									"w-[10px] h-[10px] outline-none border-0 p-0 m-0",
									fixture
										? "cursor-pointer hover:opacity-80"
										: "cursor-default",
								)}
								style={{ background }}
								onClick={(e) => {
									if (fixture) {
										selectFixtureById(fixture.id, { shift: e.shiftKey });
									}
								}}
							/>
						);
					})}
				</div>
			</div>
		</div>
	);
}
