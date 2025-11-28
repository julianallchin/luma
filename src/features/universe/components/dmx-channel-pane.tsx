import { useEffect, useMemo, useState } from "react";
import type { FixtureDefinition } from "@/bindings/fixtures";
import { cn } from "@/shared/lib/utils";
import { dmxStore } from "@/features/visualizer/stores/dmx-store";
import { useFixtureStore } from "../stores/use-fixture-store";

interface ChannelRow {
	index: number; // 0-based within fixture
	label: string;
	address: number; // absolute DMX address (1-based)
}

export function DmxChannelPane() {
	const {
		selectedPatchedId,
		patchedFixtures,
		getDefinition,
	} = useFixtureStore();
	const fixture = patchedFixtures.find((f) => f.id === selectedPatchedId);

	const [definition, setDefinition] = useState<FixtureDefinition | null>(null);
	const [values, setValues] = useState<number[]>([]);

	// Load definition when fixture changes
	useEffect(() => {
		if (!fixture) {
			setDefinition(null);
			return;
		}
		getDefinition(fixture.fixturePath).then(setDefinition);
	}, [fixture, getDefinition]);

	// Build ordered channel list for the active mode
	const channels: ChannelRow[] = useMemo(() => {
		if (!fixture) return [];
		const startAddr = Number(fixture.address);
		const count = Number(fixture.numChannels);

		const mode = definition?.Mode.find(
			(m) => m["@Name"] === fixture.modeName,
		);
		const channelNames = mode?.Channel?.map((mc) => mc["$value"]) ?? [];

		return Array.from({ length: count }).map((_, idx) => {
			const label = channelNames[idx]
				? `${idx + 1}. ${channelNames[idx]}`
				: `Channel ${idx + 1}`;
			return {
				index: idx,
				label,
				address: startAddr + idx,
			};
		});
	}, [definition, fixture]);

	// Poll DMX universe to reflect updates/overrides in UI
	useEffect(() => {
		let rafId: number;
		const tick = () => {
			if (fixture) {
				const universe = Number(fixture.universe);
				const data = dmxStore.getUniverse(universe);
				if (data) {
					const start = Number(fixture.address) - 1;
					const count = Number(fixture.numChannels);
					setValues(Array.from(data.slice(start, start + count)));
				}
			}
			rafId = requestAnimationFrame(tick);
		};
		tick();
		return () => cancelAnimationFrame(rafId);
	}, [fixture]);

	if (!fixture) {
		return (
			<div className="h-1/2 bg-card/40 border-l border-border flex flex-col">
				<header className="px-3 py-2 border-b border-border text-xs font-medium tracking-[0.08em] text-muted-foreground uppercase">
					DMX Overrides
				</header>
				<div className="flex-1 flex items-center justify-center text-xs text-muted-foreground/70 px-3 text-center">
					Select a patched fixture to edit DMX channels.
				</div>
			</div>
		);
	}

	const handleChange = (address: number, value: number) => {
		dmxStore.setOverride(Number(fixture.universe), address, value);
	};

	const clearOverrides = () => {
		dmxStore.clearOverride(Number(fixture.universe));
	};

	return (
		<div className="h-1/2 bg-card/40 border-l border-border flex flex-col min-h-[200px]">
			<header className="px-3 py-2 border-b border-border text-xs font-medium tracking-[0.08em] text-muted-foreground uppercase flex items-center justify-between">
				<span>DMX Overrides</span>
				<button
					type="button"
					onClick={clearOverrides}
					className="text-[10px] text-muted-foreground hover:text-foreground"
				>
					Clear
				</button>
			</header>

			<div className="flex-1 overflow-y-auto">
				{channels.length === 0 ? (
					<div className="text-xs text-muted-foreground/70 px-3 py-4">
						No channels for this mode.
					</div>
				) : (
					<div className="divide-y divide-border/60">
						{channels.map((ch) => {
							const currentVal = values[ch.index] ?? 0;
							return (
								<div
									key={ch.index}
									className="px-3 py-2 text-xs flex items-center gap-3"
								>
									<div className="w-28 truncate text-muted-foreground">
										{ch.label}
									</div>
									<div className="flex-1 flex items-center gap-2">
										<input
											type="range"
											min={0}
											max={255}
											value={currentVal}
											onChange={(e) =>
												handleChange(
													ch.address,
													Number.parseInt(e.target.value, 10),
												)
											}
											className="flex-1 accent-primary"
										/>
										<input
											type="number"
											min={0}
											max={255}
											value={currentVal}
											onChange={(e) =>
												handleChange(
													ch.address,
													Number.parseInt(e.target.value || "0", 10),
												)
											}
											className={cn(
												"w-14 h-8 rounded border border-border bg-background px-2 text-right font-mono text-[11px]",
											)}
										/>
									</div>
									<div className="w-10 text-right font-mono text-[10px] text-muted-foreground">
										@{ch.address}
									</div>
								</div>
							);
						})}
					</div>
				)}
			</div>
		</div>
	);
}
