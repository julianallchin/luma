export function AssignmentMatrix() {
    return (
         <div className="w-full h-full bg-background p-4 overflow-auto">
            <h3 className="text-xs font-semibold mb-2 text-muted-foreground">DMX Patch</h3>
            <div className="grid grid-cols-[repeat(auto-fill,minmax(30px,1fr))] gap-1">
                {Array.from({ length: 512 }).map((_, i) => (
                    <div key={i} className="aspect-square border border-border rounded-sm flex items-center justify-center text-[10px] text-muted-foreground hover:bg-accent hover:text-accent-foreground cursor-pointer">
                        {i + 1}
                    </div>
                ))}
            </div>
        </div>
    );
}
