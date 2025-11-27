import { Move } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import type { FixtureEntry, Mode } from "@/bindings/fixtures";
import { Button } from "@/shared/components/ui/button";
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

export function SourcePane() {
	const {
		searchQuery,
		searchResults,
		search,
		loadMore,
		hasMore,
		isSearching,
		selectFixture,
		selectedEntry,
		selectedDefinition,
		isLoadingDefinition,
	} = useFixtureStore();

	const [localQuery, setLocalQuery] = useState(searchQuery);
	const [selectedMode, setSelectedMode] = useState<string | null>(null);
	const listRef = useRef<HTMLDivElement>(null);

	// Reset mode when definition changes
	useEffect(() => {
		if (selectedDefinition && selectedDefinition.Mode.length > 0) {
			setSelectedMode(selectedDefinition.Mode[0]["@Name"]);
		} else {
			setSelectedMode(null);
		}
	}, [selectedDefinition]);

	// Debounce search
	useEffect(() => {
		const timer = setTimeout(() => {
			search(localQuery, true);
		}, 300);
		return () => clearTimeout(timer);
	}, [localQuery, search]);

	// Infinite Scroll
	const handleScroll = (e: React.UIEvent<HTMLDivElement>) => {
		const { scrollTop, scrollHeight, clientHeight } = e.currentTarget;
		// Load more when within 200px of bottom
		if (
			scrollHeight - scrollTop - clientHeight < 200 &&
			hasMore &&
			!isSearching
		) {
			loadMore();
		}
	};

	// Group results by Manufacturer
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

	return (
		<div className="flex flex-col h-full">
			{/* Search Header */}
			<div className="p-3 border-b border-border flex-shrink-0">
				<Input
					placeholder="Search fixtures..."
					value={localQuery}
					onChange={(e) => setLocalQuery(e.target.value)}
				/>
			</div>

			{/* Inventory List */}
			<div
				className="flex-1 overflow-y-auto"
				onScroll={handleScroll}
				ref={listRef}
			>
				{groupedResults.map(([manufacturer, fixtures]) => (
					<div key={manufacturer}>
						<div className="sticky top-0 z-10 bg-background/95 backdrop-blur-sm px-4 py-1 text-xs font-semibold text-muted-foreground border-b border-border/50">
							{manufacturer}
						</div>
						<div>
							{fixtures.map((fixture) => (
								<button
									key={fixture.path}
									type="button"
									className={cn(
										"w-full text-left px-4 py-1.5 pl-8 text-sm cursor-pointer hover:bg-input border-l-2 border-transparent transition-colors duration-75 bg-transparent border-none",
										selectedEntry?.path === fixture.path
											? "bg-muted border-primary"
											: "",
									)}
									onClick={() => selectFixture(fixture)}
								>
									<div className="font-medium truncate" title={fixture.model}>
										{fixture.model}
									</div>
								</button>
							))}
						</div>
					</div>
				))}

				{isSearching && searchResults.length > 0 && (
					<div className="p-2 text-center text-xs text-muted-foreground animate-pulse">
						Loading more...
					</div>
				)}

				{!isSearching && searchResults.length === 0 && (
					<div className="p-4 text-center text-xs text-muted-foreground">
						No fixtures found.
					</div>
				)}
			</div>

			{/* Configuration Dock */}
			<div className="min-h-[150px] border-t border-border p-4 flex flex-col flex-shrink-0 bg-muted/20">
				<h3 className="text-xs font-semibold uppercase text-muted-foreground mb-2">
					Configuration
				</h3>
				{selectedEntry ? (
					isLoadingDefinition ? (
						<div className="text-xs text-muted-foreground animate-pulse">
							Loading definition...
						</div>
					) : selectedDefinition ? (
						<div className="flex flex-col gap-3 flex-1">
							<div className="text-sm font-medium truncate">
								<span className="opacity-70">
									{selectedDefinition.Manufacturer}
								</span>{" "}
								<span className="font-bold">{selectedDefinition.Model}</span>
							</div>

							<div className="flex flex-col gap-1.5">
								<label
									htmlFor="mode-select"
									className="text-[10px] uppercase font-semibold text-muted-foreground"
								>
									Mode
								</label>
								<Select
									value={selectedMode || ""}
									onValueChange={setSelectedMode}
								>
									<SelectTrigger
										id="mode-select"
										className="h-8 text-xs w-full"
									>
										<SelectValue placeholder="Select Mode" />
									</SelectTrigger>
									<SelectContent>
										{selectedDefinition.Mode.map((mode: Mode) => (
											<SelectItem key={mode["@Name"]} value={mode["@Name"]}>
												{mode["@Name"]} ({mode.Channel?.length || 0}ch)
											</SelectItem>
										))}
									</SelectContent>
								</Select>
							</div>

							<div className="flex-1" />

							<Button
								type="button"
								className="mt-auto bg-[#333] hover:bg-[#444] border border-[#555] text-white text-sm py-3 px-4 rounded cursor-grab active:cursor-grabbing flex items-center justify-center gap-2"
								draggable
								onDragStart={(e) => {
									const modeName =
										selectedMode || selectedDefinition.Mode[0]["@Name"];
									const mode = selectedDefinition.Mode.find(
										(m) => m["@Name"] === modeName,
									);
									const channels = mode?.Channel?.length || 0;
									const payload = JSON.stringify({
										modeName,
										numChannels: channels,
									});
									console.debug("[SourcePane] dragstart", {
										modeName,
										channels,
										types: e.dataTransfer.types,
									});
									// Keep a JS-side copy in case the webview strips DataTransfer types.
									(
										window as unknown as Record<string, unknown>
									).__lumaDragPayload = payload;
									// Some webviews only allow plain text; set both.
									e.dataTransfer.setData("application/json", payload);
									e.dataTransfer.setData("text/plain", payload);
									e.dataTransfer.effectAllowed = "copy";
								}}
							>
								<Move size={16} />
								Drag to grid
							</Button>
						</div>
					) : (
						<div className="text-xs text-red-400">Failed to load</div>
					)
				) : (
					<div className="text-xs text-muted-foreground italic">
						Select a fixture to configure
					</div>
				)}
			</div>
		</div>
	);
}
