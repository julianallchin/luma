import { useEffect, useId, useMemo, useRef, useState } from "react";
import type { FixtureEntry, Mode, PatchedFixture } from "@/bindings/fixtures";
import { Button } from "@/shared/components/ui/button";
import {
	Dialog,
	DialogContent,
	DialogHeader,
	DialogTitle,
} from "@/shared/components/ui/dialog";
import { Input } from "@/shared/components/ui/input";
import {
	Select,
	SelectContent,
	SelectItem,
	SelectTrigger,
	SelectValue,
} from "@/shared/components/ui/select";
import { cn } from "@/shared/lib/utils";
import { useFixtureStore } from "../stores/use-fixture-store";

function findNextAvailableAddress(
	patched: PatchedFixture[],
	numChannels: number,
): number | null {
	const ranges = patched
		.map((f) => ({
			start: Number(f.address),
			end: Number(f.address) + Number(f.numChannels) - 1,
		}))
		.sort((a, b) => a.start - b.start);

	let candidate = 1;
	for (const r of ranges) {
		if (candidate + numChannels - 1 < r.start) return candidate;
		candidate = Math.max(candidate, r.end + 1);
	}
	if (candidate + numChannels - 1 <= 512) return candidate;
	return null;
}

export function AddFixtureDialog({
	open,
	onOpenChange,
}: {
	open: boolean;
	onOpenChange: (next: boolean) => void;
}) {
	const searchResults = useFixtureStore((s) => s.searchResults);
	const search = useFixtureStore((s) => s.search);
	const loadMore = useFixtureStore((s) => s.loadMore);
	const hasMore = useFixtureStore((s) => s.hasMore);
	const isSearching = useFixtureStore((s) => s.isSearching);
	const selectFixture = useFixtureStore((s) => s.selectFixture);
	const selectedEntry = useFixtureStore((s) => s.selectedEntry);
	const selectedDefinition = useFixtureStore((s) => s.selectedDefinition);
	const isLoadingDefinition = useFixtureStore((s) => s.isLoadingDefinition);
	const patchFixture = useFixtureStore((s) => s.patchFixture);
	const patchedFixtures = useFixtureStore((s) => s.patchedFixtures);

	const [localQuery, setLocalQuery] = useState("");
	const [selectedMode, setSelectedMode] = useState<string | null>(null);
	const modeSelectId = useId();
	const listRef = useRef<HTMLDivElement>(null);

	useEffect(() => {
		if (selectedDefinition && selectedDefinition.Mode.length > 0) {
			setSelectedMode(selectedDefinition.Mode[0]["@Name"]);
		} else {
			setSelectedMode(null);
		}
	}, [selectedDefinition]);

	useEffect(() => {
		if (!open) return;
		const timer = setTimeout(() => {
			search(localQuery, true);
		}, 300);
		return () => clearTimeout(timer);
	}, [localQuery, search, open]);

	const handleScroll = (e: React.UIEvent<HTMLDivElement>) => {
		const { scrollTop, scrollHeight, clientHeight } = e.currentTarget;
		if (
			scrollHeight - scrollTop - clientHeight < 200 &&
			hasMore &&
			!isSearching
		) {
			loadMore();
		}
	};

	const groupedResults = useMemo(() => {
		const groups: Record<string, FixtureEntry[]> = {};
		for (const fixture of searchResults) {
			if (!groups[fixture.manufacturer]) {
				groups[fixture.manufacturer] = [];
			}
			groups[fixture.manufacturer].push(fixture);
		}
		return Object.entries(groups).sort((a, b) => a[0].localeCompare(b[0]));
	}, [searchResults]);

	const handleAdd = async () => {
		if (!selectedEntry || !selectedDefinition) return;
		const modeName = selectedMode || selectedDefinition.Mode[0]["@Name"];
		const mode = selectedDefinition.Mode.find((m) => m["@Name"] === modeName);
		const channels = mode?.Channel?.length ?? 0;
		if (channels <= 0) return;
		const address = findNextAvailableAddress(patchedFixtures, channels);
		if (address === null) {
			console.error("No available address for new fixture");
			return;
		}
		await patchFixture(1, address, modeName, channels);
		onOpenChange(false);
	};

	const canAdd =
		!!selectedEntry && !!selectedDefinition && !isLoadingDefinition;

	return (
		<Dialog open={open} onOpenChange={onOpenChange}>
			<DialogContent className="max-w-2xl h-[600px] p-0 gap-0 flex flex-col">
				<DialogHeader className="px-3 h-8 flex flex-row items-center border-b border-trim shrink-0">
					<DialogTitle className="text-[9px] uppercase tracking-wider font-bold text-foreground/90">
						Add Fixture
					</DialogTitle>
				</DialogHeader>

				<div className="flex flex-1 min-h-0">
					{/* Left: search + list */}
					<div className="flex flex-col w-1/2 border-r border-trim min-w-0">
						<div className="p-2 border-b border-trim shrink-0">
							<Input
								placeholder="Search fixtures..."
								value={localQuery}
								onChange={(e) => setLocalQuery(e.target.value)}
							/>
						</div>
						<div
							className="flex-1 overflow-y-auto"
							onScroll={handleScroll}
							ref={listRef}
						>
							{groupedResults.map(([manufacturer, fixtures]) => (
								<div key={manufacturer}>
									<div className="sticky top-0 z-10 bg-card px-3 py-1 text-[9px] uppercase tracking-wider font-bold text-foreground/60 border-b border-trim">
										{manufacturer}
									</div>
									<div>
										{fixtures.map((fixture) => (
											<button
												key={fixture.path}
												type="button"
												className={cn(
													"w-full text-left px-3 py-1 text-[11px] cursor-pointer bg-transparent border-0 border-l-2 border-transparent hover:bg-hover transition-colors",
													selectedEntry?.path === fixture.path &&
														"bg-primary/10 border-primary",
												)}
												onClick={() => selectFixture(fixture)}
											>
												<div className="truncate" title={fixture.model}>
													{fixture.model}
												</div>
											</button>
										))}
									</div>
								</div>
							))}
							{isSearching && searchResults.length > 0 && (
								<div className="p-2 text-center text-[10px] text-foreground/40">
									Loading more...
								</div>
							)}
							{!isSearching && searchResults.length === 0 && (
								<div className="p-3 text-center text-[10px] text-foreground/40">
									No fixtures found.
								</div>
							)}
						</div>
					</div>

					{/* Right: configuration */}
					<div className="flex flex-col w-1/2 min-w-0">
						<div className="px-3 h-6 flex items-center border-b border-trim text-[9px] uppercase tracking-wider font-bold text-foreground/70 shrink-0">
							Configure
						</div>
						<div className="flex-1 p-3 flex flex-col gap-3 min-h-0">
							{selectedEntry ? (
								isLoadingDefinition ? (
									<div className="text-[10px] uppercase tracking-wider text-foreground/50">
										Loading definition...
									</div>
								) : selectedDefinition ? (
									<>
										<div className="text-xs">
											<div className="text-foreground/60 truncate">
												{selectedDefinition.Manufacturer}
											</div>
											<div className="text-foreground font-bold truncate">
												{selectedDefinition.Model}
											</div>
										</div>
										<div className="flex flex-col gap-1.5">
											<label
												htmlFor={modeSelectId}
												className="text-[9px] uppercase tracking-wider font-bold text-foreground/70"
											>
												Mode
											</label>
											<Select
												value={selectedMode || ""}
												onValueChange={setSelectedMode}
											>
												<SelectTrigger
													id={modeSelectId}
													className="h-6 text-[10px] w-full rounded-none"
												>
													<SelectValue placeholder="Select Mode" />
												</SelectTrigger>
												<SelectContent>
													{selectedDefinition.Mode.map((mode: Mode) => (
														<SelectItem
															key={mode["@Name"]}
															value={mode["@Name"]}
														>
															{mode["@Name"]} ({mode.Channel?.length || 0}ch)
														</SelectItem>
													))}
												</SelectContent>
											</Select>
										</div>
									</>
								) : (
									<div className="text-[10px] text-destructive uppercase tracking-wider">
										Failed to load
									</div>
								)
							) : (
								<div className="text-[10px] uppercase tracking-wider text-foreground/40">
									Select a fixture from the list
								</div>
							)}
						</div>
						<div className="flex items-center justify-end gap-2 px-3 h-10 border-t border-trim shrink-0">
							<Button onClick={() => onOpenChange(false)}>Cancel</Button>
							<Button onClick={handleAdd} disabled={!canAdd}>
								Add
							</Button>
						</div>
					</div>
				</div>
			</DialogContent>
		</Dialog>
	);
}
