export function AssignmentMatrix() {
    return (
         <div className="w-full h-full bg-background p-4 overflow-auto">
            <h3 className="text-xs font-semibold mb-2 text-muted-foreground">DMX Patch</h3>
            <div className="grid grid-cols-[repeat(auto-fill,minmax(30px,1fr))]">
                {Array.from({ length: 512 }).map((_, i) => (
                    <div key={i} className="aspect-square border border-border/20 flex items-center justify-center text-[9px] text-muted-foreground/50 select-none">
                        {i + 1}
                    </div>
                ))}
            </div>
        </div>
    );
}
