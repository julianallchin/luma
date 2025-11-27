import { useEffect, useState } from 'react';
import { useFixtureStore } from '../stores/use-fixture-store';
import { cn } from '@/shared/lib/utils';
// The Mode type is part of FixtureDefinition, but if we need it explicitly:
import type { Mode } from '@/bindings/fixtures';

export function SourcePane() {
    const { searchQuery, searchResults, search, selectFixture, selectedEntry, selectedDefinition, isLoadingDefinition } = useFixtureStore();
    const [localQuery, setLocalQuery] = useState(searchQuery);

    // Debounce search
    useEffect(() => {
        const timer = setTimeout(() => {
            search(localQuery);
        }, 300);
        return () => clearTimeout(timer);
    }, [localQuery, search]);

    return (
        <div className="flex flex-col h-full">
             {/* Search Header */}
            <div className="p-3 border-b border-border">
                <input
                    type="text"
                    placeholder="Search fixtures..."
                    className="w-full px-3 py-1.5 bg-secondary text-sm rounded-md border border-transparent focus:border-primary outline-none"
                    value={localQuery}
                    onChange={(e) => setLocalQuery(e.target.value)}
                />
            </div>

            {/* Inventory List */}
            <div className="flex-1 overflow-y-auto">
                {searchResults.map((fixture) => (
                    <div
                        key={fixture.path}
                        className={cn(
                            "px-4 py-2 text-sm cursor-pointer hover:bg-secondary/50 border-l-2 border-transparent transition-colors",
                             selectedEntry?.path === fixture.path ? "bg-secondary border-primary" : ""
                        )}
                        onClick={() => selectFixture(fixture)}
                    >
                        <div className="font-medium">{fixture.manufacturer}</div>
                        <div className="text-muted-foreground text-xs">{fixture.model}</div>
                    </div>
                ))}
                {searchResults.length === 0 && (
                    <div className="p-4 text-center text-xs text-muted-foreground">
                        No fixtures found.
                    </div>
                )}
            </div>

            {/* Configuration Dock */}
            <div className="h-[25%] min-h-[150px] border-t border-border p-4 bg-secondary/10 flex flex-col">
                <h3 className="text-xs font-semibold uppercase text-muted-foreground mb-2">Configuration</h3>
                {selectedEntry ? (
                    isLoadingDefinition ? (
                        <div className="text-xs text-muted-foreground animate-pulse">Loading definition...</div>
                    ) : selectedDefinition ? (
                        <div className="flex flex-col gap-2">
                            <div className="text-sm font-medium">{selectedDefinition.Manufacturer} {selectedDefinition.Model}</div>
                            
                            <label className="text-xs text-muted-foreground">Mode</label>
                            <select className="w-full bg-background border border-border rounded px-2 py-1 text-sm">
                                {selectedDefinition.Mode.map((mode: Mode) => (
                                    <option key={mode["@Name"]} value={mode["@Name"]}>
                                        {mode["@Name"]} ({mode.Channel?.length || 0}ch)
                                    </option>
                                ))}
                            </select>
                            
                            <div className="mt-2 p-2 border border-dashed border-border rounded flex items-center justify-center text-xs text-muted-foreground cursor-grab active:cursor-grabbing hover:bg-accent/5">
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
