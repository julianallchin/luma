import { useState } from 'react';
import { useFixtureStore } from '../stores/use-fixture-store';
import { cn } from '@/shared/lib/utils';
import type { PatchedFixture } from '@/bindings/fixtures';

export function AssignmentMatrix() {
    const { patchedFixtures, patchFixture, removePatchedFixture } = useFixtureStore();
    const [hoverState, setHoverState] = useState<{ address: number, numChannels: number, valid: boolean } | null>(null);

    // Handle drag over to show preview
    const handleDragOver = (e: React.DragEvent, address: number) => {
        e.preventDefault();
        try {
            const data = JSON.parse(e.dataTransfer.getData("application/json") || "{}");
            if (data.numChannels) {
                const numChannels = data.numChannels;
                const endAddress = address + numChannels - 1;
                
                // Check bounds
                if (endAddress > 512) {
                    setHoverState({ address, numChannels, valid: false });
                    return;
                }

                // Check overlap
                const hasOverlap = patchedFixtures.some(f => {
                    const fEnd = f.address + f.numChannels - 1;
                    return (address <= fEnd && endAddress >= f.address);
                });

                setHoverState({ address, numChannels, valid: !hasOverlap });
            }
        } catch (err) {
            // Data transfer might not be available during dragover in some browsers
        }
    };

    const handleDrop = async (e: React.DragEvent, address: number) => {
        e.preventDefault();
        setHoverState(null);
        
        try {
            const data = JSON.parse(e.dataTransfer.getData("application/json"));
            if (data.modeName && data.numChannels) {
                // Re-validate
                const endAddress = address + data.numChannels - 1;
                 if (endAddress > 512) return;

                 const hasOverlap = patchedFixtures.some(f => {
                    const fEnd = f.address + f.numChannels - 1;
                    return (address <= fEnd && endAddress >= f.address);
                });

                if (!hasOverlap) {
                    await patchFixture(1, address, data.modeName, data.numChannels);
                }
            }
        } catch (err) {
            console.error("Drop failed", err);
        }
    };

    // Helper to render patched fixtures
    const renderCellContent = (i: number) => {
        const address = i + 1;
        
        // Check if this cell is the start of a patched fixture
        const fixture = patchedFixtures.find(f => f.address === address);
        if (fixture) {
            return (
                <div 
                    className="absolute inset-0 z-10 bg-primary/20 border border-primary text-primary-foreground text-[10px] flex flex-col items-center justify-center overflow-hidden select-none"
                    style={{ 
                        width: `calc(100% * ${fixture.numChannels} + ${(fixture.numChannels - 1) * 0}px)`, // Simplified width, grid has no gaps
                        zIndex: 20
                    }}
                    title={`${fixture.manufacturer} ${fixture.model} (${fixture.modeName})`}
                    onContextMenu={(e) => {
                        e.preventDefault();
                        if(confirm(`Unpatch ${fixture.model}?`)) {
                            removePatchedFixture(fixture.id);
                        }
                    }}
                >
                    <span className="font-bold truncate w-full text-center px-1">{fixture.model}</span>
                    <span className="text-[8px] opacity-70">{fixture.address} - {fixture.address + fixture.numChannels - 1}</span>
                </div>
            );
        }

        // Check if occupied by a fixture (but not start)
        const isOccupied = patchedFixtures.some(f => address > f.address && address < f.address + f.numChannels);
        if (isOccupied) return null; // Covered by the main block

        return (
            <span className="text-[9px] text-muted-foreground/50 select-none">{address}</span>
        );
    };

    return (
         <div className="w-full h-full bg-background p-4 overflow-auto">
            <h3 className="text-xs font-semibold mb-2 text-muted-foreground">DMX Patch (Universe 1)</h3>
            <div className="grid grid-cols-[repeat(auto-fill,minmax(30px,1fr))] relative">
                {Array.from({ length: 512 }).map((_, i) => {
                    const address = i + 1;
                    
                    // Check hover state
                    let highlightClass = "";
                    if (hoverState) {
                        const endHover = hoverState.address + hoverState.numChannels - 1;
                        if (address >= hoverState.address && address <= endHover) {
                            highlightClass = hoverState.valid ? "bg-green-500/30" : "bg-red-500/30";
                        }
                    }

                    return (
                        <div 
                            key={i} 
                            className={cn(
                                "aspect-square border border-border/20 flex items-center justify-center relative",
                                highlightClass
                            )}
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
