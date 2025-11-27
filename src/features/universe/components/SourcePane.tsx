import { useEffect, useState, useMemo, useRef } from 'react';
import { useFixtureStore } from '../stores/use-fixture-store';
import { cn } from '@/shared/lib/utils';
import type { Mode, FixtureEntry } from '@/bindings/fixtures';
import {
    Select,
    SelectContent,
    SelectItem,
    SelectTrigger,
    SelectValue,
} from "@/shared/components/ui/select";

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
        isLoadingDefinition 
    } = useFixtureStore();
    
    const [localQuery, setLocalQuery] = useState(searchQuery);
    const listRef = useRef<HTMLDivElement>(null);

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
        if (scrollHeight - scrollTop - clientHeight < 200 && hasMore && !isSearching) {
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
                <input
                    type="text"
                    placeholder="Search fixtures..."
                    className="w-full px-3 py-1.5 bg-secondary text-sm rounded-md border border-transparent focus:border-primary outline-none"
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
                                <div
                                    key={fixture.path}
                                    className={cn(
                                        "px-4 py-1.5 pl-8 text-sm cursor-pointer hover:bg-secondary/50 border-l-2 border-transparent transition-colors",
                                        selectedEntry?.path === fixture.path ? "bg-secondary border-primary" : ""
                                    )}
                                    onClick={() => selectFixture(fixture)}
                                >
                                    <div className="font-medium truncate" title={fixture.model}>
                                        {fixture.model}
                                    </div>
                                </div>
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
            <div className="h-[25%] min-h-[150px] border-t border-border p-4 bg-secondary/10 flex flex-col flex-shrink-0">
                <h3 className="text-xs font-semibold uppercase text-muted-foreground mb-2">Configuration</h3>
                {selectedEntry ? (
                    isLoadingDefinition ? (
                        <div className="text-xs text-muted-foreground animate-pulse">Loading definition...</div>
                    ) : selectedDefinition ? (
                        <div className="flex flex-col gap-3">
                            <div className="text-sm font-medium truncate">
                                <span className="opacity-70">{selectedDefinition.Manufacturer}</span> <span className="font-bold">{selectedDefinition.Model}</span>
                            </div>
                            
                            <div className="flex flex-col gap-1.5">
                                <label className="text-[10px] uppercase font-semibold text-muted-foreground">Mode</label>
                                <Select>
                                    <SelectTrigger className="h-8 text-xs">
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
                            
                            <div className="mt-auto p-2 border border-dashed border-border rounded flex items-center justify-center text-xs text-muted-foreground cursor-grab active:cursor-grabbing hover:bg-accent/5 select-none">
                                Drag to Patch
                            </div>
                        </div>
                    ) : (
                         <div className="text-xs text-red-400">Failed to load</div>
                    )
                ) : (
                    <div className="text-xs text-muted-foreground italic">Select a fixture to configure</div>
                )}
            </div>
        </div>
    );
}
