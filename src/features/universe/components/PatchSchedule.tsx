import { useEffect } from "react";
import { useFixtureStore } from "../stores/use-fixture-store";
import { cn } from "@/shared/lib/utils";

export function PatchSchedule() {
	const {
		patchedFixtures,
		removePatchedFixture,
		selectedPatchedId,
		setSelectedPatchedId,
	} = useFixtureStore();

	useEffect(() => {
		const handleKey = (e: KeyboardEvent) => {
			if ((e.key === "Delete" || e.key === "Backspace") && selectedPatchedId) {
				const target = e.target as HTMLElement | null;
				if (
					target &&
					(["INPUT", "TEXTAREA"].includes(target.tagName) ||
						target.isContentEditable)
				) {
					return;
				}
				e.preventDefault();
				removePatchedFixture(selectedPatchedId);
			}
		};
		window.addEventListener("keydown", handleKey);
		return () => window.removeEventListener("keydown", handleKey);
	}, [removePatchedFixture, selectedPatchedId]);

	return (
		<div className="w-80 bg-card/30 border-l border-border flex flex-col h-full">
			<div className="px-3 py-2 border-b border-border text-[10px] font-semibold tracking-[0.08em] text-muted-foreground uppercase">
				Patch Schedule
			</div>

			<div className="flex-1 overflow-y-auto">
				{patchedFixtures.length === 0 ? (
					<div className="text-xs text-muted-foreground/60 px-3 py-6 text-center">
						No patched fixtures
					</div>
				) : (
					<div className="divide-y divide-border/60">
						{patchedFixtures.map((fixture, index) => (
							<div
								key={fixture.id}
								className={cn(
									"grid grid-cols-[28px_minmax(0,1fr)_84px_60px] items-center gap-2 px-3 py-2 text-[11px] transition-colors cursor-pointer relative",
									selectedPatchedId === fixture.id
										? "bg-primary/10"
										: "hover:bg-card",
								)}
								onClick={() => setSelectedPatchedId(fixture.id)}
								title={`${fixture.manufacturer} ${fixture.model} • ${fixture.modeName ?? ""} @ ${fixture.address}`}
							>
								<span className="text-[10px] text-muted-foreground">
									{index + 1}
								</span>
								<span className="truncate text-xs font-medium text-foreground">
									{fixture.label ?? fixture.model}
								</span>
								<span className="truncate text-[11px] text-muted-foreground">
									{fixture.modeName}
								</span>
								<span className="text-right font-mono text-[10px] text-muted-foreground">
									{fixture.address}
								</span>
							</div>
						))}
					</div>
				)}
			</div>
		</div>
	);
}
