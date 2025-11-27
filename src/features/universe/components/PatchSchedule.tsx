import { useEffect, useRef, useState } from "react";
import { cn } from "@/shared/lib/utils";
import { useFixtureStore } from "../stores/use-fixture-store";

export function PatchSchedule() {
	const {
		patchedFixtures,
		removePatchedFixture,
		selectedPatchedId,
		setSelectedPatchedId,
		updatePatchedFixtureLabel,
	} = useFixtureStore();
	const [editingId, setEditingId] = useState<string | null>(null);
	const [editingValue, setEditingValue] = useState("");
	const inputRef = useRef<HTMLInputElement | null>(null);

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

	useEffect(() => {
		if (editingId && inputRef.current) {
			inputRef.current.focus();
			inputRef.current.select();
		}
	}, [editingId]);

	const startEditing = (fixtureId: string, label: string) => {
		setEditingId(fixtureId);
		setEditingValue(label);
		setSelectedPatchedId(fixtureId);
	};

	const commitEdit = async () => {
		if (!editingId) return;
		const next = editingValue.trim();
		if (!next) {
			setEditingId(null);
			return;
		}

		const current = patchedFixtures.find((f) => f.id === editingId);
		const currentLabel = current?.label ?? current?.model ?? "";
		if (currentLabel === next) {
			setEditingId(null);
			return;
		}
		await updatePatchedFixtureLabel(editingId, next);
		setEditingId(null);
	};

	const cancelEdit = () => {
		setEditingId(null);
		setEditingValue("");
	};

	return (
		<div className="w-80 bg-card/30 border-l border-border flex flex-col h-full">
			<div className="px-3 py-2 border-b border-border text-xs font-medium tracking-[0.08em] text-muted-foreground uppercase">
				Patch Schedule
			</div>

			<div className="flex-1 overflow-y-auto">
				{patchedFixtures.length === 0 ? (
					<div className="text-xs text-muted-foreground/60 px-3 py-6 text-center">
						No patched fixtures
					</div>
				) : (
					<div className="divide-y divide-border/60 border-b border-border/60">
						<div className="grid grid-cols-[28px_minmax(0,1fr)_minmax(0,1fr)_32px_32px] items-center gap-2 px-3 py-1 text-[10px] uppercase tracking-[0.08em] text-muted-foreground bg-card/40 sticky top-0 z-10">
							<span>ID</span>
							<span>Label</span>
							<span>Fixture</span>
							<span className="text-center">Addr</span>
							<span className="text-right">Ch</span>
						</div>
						{patchedFixtures.map((fixture, index) => (
							<div
								key={fixture.id}
								className={cn(
									"grid grid-cols-[28px_minmax(0,1fr)_minmax(0,1fr)_32px_32px] items-center gap-2 px-3 py-1 text-[11px] transition-colors cursor-pointer relative",
									selectedPatchedId === fixture.id
										? "bg-primary/10"
										: "hover:bg-card",
								)}
								onClick={() => setSelectedPatchedId(fixture.id)}
								title={`${fixture.manufacturer} ${fixture.model} • ${fixture.modeName ?? ""} @ ${fixture.address} (${fixture.numChannels}ch)`}
							>
								<span className="text-[10px] text-muted-foreground">
									{index + 1}
								</span>
								{editingId === fixture.id ? (
									<input
										ref={inputRef}
										value={editingValue}
										onChange={(e) => setEditingValue(e.target.value)}
										onBlur={commitEdit}
										onKeyDown={(e) => {
											if (e.key === "Enter") {
												e.preventDefault();
												void commitEdit();
											} else if (e.key === "Escape") {
												e.preventDefault();
												cancelEdit();
											}
										}}
										className="w-full truncate text-xs font-medium text-foreground bg-transparent border-none outline-none focus:outline-none focus:ring-0"
									/>
								) : (
									<span
										className="truncate text-xs font-medium text-foreground"
										onDoubleClick={(e) => {
											e.stopPropagation();
											startEditing(
												fixture.id,
												fixture.label ?? fixture.model ?? "",
											);
										}}
									>
										{fixture.label ?? fixture.model}
									</span>
								)}
								<span className="truncate text-[11px] text-muted-foreground">
									{fixture.model}
								</span>
								<span className="text-center font-mono text-[10px] text-muted-foreground">
									{fixture.address}
								</span>
								<span className="text-right font-mono text-[10px] text-muted-foreground">
									{fixture.numChannels}
								</span>
							</div>
						))}
					</div>
				)}
			</div>
		</div>
	);
}
