import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useEffect, useState } from "react";
import type { FixtureGroup } from "@/bindings/groups";
import type {
	ControllerStatus,
	Cue,
	MidiBinding,
	MidiInput,
	ModifierDef,
} from "@/bindings/midi";
import type { PatternArgDef } from "@/bindings/schema";
import { Button } from "@/shared/components/ui/button";
import { Input } from "@/shared/components/ui/input";
import {
	NativeSelect,
	NativeSelectOption,
} from "@/shared/components/ui/native-select";
import {
	Popover,
	PopoverContent,
	PopoverTrigger,
} from "@/shared/components/ui/popover";
import {
	Select,
	SelectContent,
	SelectItem,
	SelectTrigger,
	SelectValue,
} from "@/shared/components/ui/select";
import {
	ColorPicker,
	ColorPickerCopyPaste,
	ColorPickerHue,
	ColorPickerSelection,
} from "@/shared/components/ui/shadcn-io/color-picker";

// ─── helpers ──────────────────────────────────────────────────────────────────

export function labelMidiInput(input: MidiInput): string {
	const ch = `Ch${input.channel + 1}`;
	if (input.type === "note") return `Note ${input.note} ${ch}`;
	if (input.type === "controlChange") return `CC ${input.cc} ${ch}`;
	return `CC ${input.cc} (value) ${ch}`;
}

export function labelAction(
	action: MidiBinding["action"],
	cues: Cue[],
	groups: FixtureGroup[],
): string {
	if (action.type === "fireCue") {
		const cue = cues.find((c) => c.id === action.cue_id);
		return `Fire "${cue?.name ?? "?"}"`;
	}
	if (action.type === "setIntensity") {
		if (!action.group_id) return "Master brightness";
		const g = groups.find((g) => g.id === action.group_id);
		return `${g?.name ?? action.group_id} brightness`;
	}
	if (action.type === "blackout") return "Blackout";
	return "Controller on/off";
}

export function labelMode(mode: MidiBinding["mode"]): string {
	if (mode.type === "toggle") return "Toggle";
	if (mode.type === "flash") return "Flash";
	return "Tap/hold";
}

export function argDefaultValue(arg: PatternArgDef): unknown {
	if (arg.argType === "Color") {
		const d = arg.defaultValue as Record<string, unknown> | null | undefined;
		return d ?? { r: 255, g: 0, b: 0, a: 1 };
	}
	if (arg.argType === "Scalar") {
		return typeof arg.defaultValue === "number" ? arg.defaultValue : 1.0;
	}
	// Selection
	const d = arg.defaultValue as Record<string, unknown> | null | undefined;
	return d ?? { expression: "all", spatialReference: "global" };
}

export function hexFromColor(val: unknown): string {
	const v = val as Record<string, number> | null | undefined;
	if (!v) return "#ff0000";
	const r = Math.round(v.r ?? 255)
		.toString(16)
		.padStart(2, "0");
	const g = Math.round(v.g ?? 0)
		.toString(16)
		.padStart(2, "0");
	const b = Math.round(v.b ?? 0)
		.toString(16)
		.padStart(2, "0");
	return `#${r}${g}${b}`;
}

// ─── device section ───────────────────────────────────────────────────────────

export function DeviceSection({
	status,
	connecting,
	error,
	onConnect,
	onDisconnect,
	onRefresh,
}: {
	status: ControllerStatus | null;
	connecting: boolean;
	error: string | null;
	onConnect: (port: string) => void;
	onDisconnect: () => void;
	onRefresh: () => void;
}) {
	return (
		<div className="space-y-2">
			{status?.connected && status.portName ? (
				<div className="flex items-center justify-between border border-border/40 bg-background/50 px-4 py-3">
					<div>
						<div className="text-sm font-medium">{status.portName}</div>
						<div className="flex items-center gap-1.5 mt-0.5">
							<div className="h-1.5 w-1.5 rounded-full bg-green-500" />
							<span className="text-xs text-muted-foreground">Connected</span>
						</div>
					</div>
					<Button variant="outline" size="sm" onClick={onDisconnect}>
						Disconnect
					</Button>
				</div>
			) : (
				<div className="space-y-1">
					{!status || status.availablePorts.length === 0 ? (
						<div className="border border-border/40 bg-background/50 p-3 text-center text-xs text-muted-foreground">
							No MIDI devices found
						</div>
					) : (
						<div className="border border-border/40 bg-background/50 divide-y divide-border/40">
							{status.availablePorts.map((port) => (
								<div
									key={port}
									className="flex items-center justify-between px-4 py-2.5"
								>
									<span className="text-sm">{port}</span>
									<Button
										size="sm"
										onClick={() => onConnect(port)}
										disabled={connecting}
									>
										{connecting ? "Connecting…" : "Connect"}
									</Button>
								</div>
							))}
						</div>
					)}
					<button
						type="button"
						onClick={onRefresh}
						className="text-xs text-muted-foreground hover:text-foreground transition-colors"
					>
						Refresh ports
					</button>
				</div>
			)}
			{error && <p className="text-xs text-destructive">{error}</p>}
		</div>
	);
}

// ─── cue args form section (used in CueEditorDialog) ─────────────────────────

export function CueArgsFields({
	patternArgs,
	argValues,
	setArgValue,
}: {
	patternArgs: PatternArgDef[];
	argValues: Record<string, unknown>;
	setArgValue: (id: string, value: unknown) => void;
}) {
	if (patternArgs.length === 0) return null;
	return (
		<div className="space-y-2 pt-1 border-t border-border/40">
			<span className="text-xs text-muted-foreground">Pattern arguments</span>
			<div className="grid grid-cols-2 gap-3">
				{patternArgs.map((arg) => {
					const val = argValues[arg.id] ?? argDefaultValue(arg);
					if (arg.argType === "Color") {
						const hex = hexFromColor(val);
						return (
							<Field key={arg.id} label={arg.name}>
								<Popover>
									<PopoverTrigger asChild>
										<button
											type="button"
											className="flex h-8 w-full items-center gap-2 border border-border/40 bg-background px-2 text-sm hover:border-border transition-colors"
										>
											<span
												className="h-4 w-4 shrink-0 rounded border border-border/40"
												style={{ backgroundColor: hex }}
											/>
											<span className="font-mono text-xs text-muted-foreground">
												{hex}
											</span>
										</button>
									</PopoverTrigger>
									<PopoverContent className="w-auto p-3" side="right">
										<ColorPicker
											value={hex}
											onChange={(rgba) => {
												if (Array.isArray(rgba) && rgba.length >= 4) {
													setArgValue(arg.id, {
														r: Math.round(Number(rgba[0])),
														g: Math.round(Number(rgba[1])),
														b: Math.round(Number(rgba[2])),
														a: Number(rgba[3]),
													});
												}
											}}
										>
											<div className="flex flex-col gap-2">
												<ColorPickerSelection className="h-28 w-48 rounded-md" />
												<ColorPickerHue className="flex-1" />
												<ColorPickerCopyPaste />
											</div>
										</ColorPicker>
									</PopoverContent>
								</Popover>
							</Field>
						);
					}
					if (arg.argType === "Scalar") {
						return (
							<Field key={arg.id} label={arg.name}>
								<Input
									type="number"
									step="0.1"
									value={typeof val === "number" ? val : 1}
									onChange={(e) => setArgValue(arg.id, Number(e.target.value))}
									className="h-8 text-sm"
								/>
							</Field>
						);
					}
					// Selection
					const expr =
						((val as Record<string, unknown>)?.expression as string) ?? "all";
					return (
						<Field key={arg.id} label={arg.name}>
							<Input
								value={expr}
								onChange={(e) =>
									setArgValue(arg.id, {
										...(val as object),
										expression: e.target.value,
									})
								}
								placeholder="e.g. front_wash"
								className="h-8 text-sm"
							/>
						</Field>
					);
				})}
			</div>
		</div>
	);
}

// ─── cue form fields (name, pattern, exec, z-index, blend) ───────────────────

export function CueBaseFields({
	name,
	setName,
	patternId,
	setPatternId,
	execMode,
	setExecMode,
	zIndex,
	setZIndex,
	blendMode,
	setBlendMode,
	patterns,
}: {
	name: string;
	setName: (v: string) => void;
	patternId: string;
	setPatternId: (v: string) => void;
	execMode: string;
	setExecMode: (v: string) => void;
	zIndex: number;
	setZIndex: (v: number) => void;
	blendMode: string;
	setBlendMode: (v: string) => void;
	patterns: { id: string; name: string }[];
}) {
	return (
		<div className="grid grid-cols-2 gap-3">
			<Field label="Name">
				<Input
					value={name}
					onChange={(e) => setName(e.target.value)}
					placeholder="e.g. Strobe"
					className="h-8 text-sm"
					autoFocus
				/>
			</Field>
			<Field label="Pattern">
				<Select value={patternId} onValueChange={setPatternId}>
					<SelectTrigger className="h-8 text-sm">
						<SelectValue />
					</SelectTrigger>
					<SelectContent>
						{patterns.map((p) => (
							<SelectItem key={p.id} value={p.id} className="text-sm">
								{p.name}
							</SelectItem>
						))}
					</SelectContent>
				</Select>
			</Field>
			<Field label="Execution">
				<Select value={execMode} onValueChange={setExecMode}>
					<SelectTrigger className="h-8 text-sm">
						<SelectValue />
					</SelectTrigger>
					<SelectContent>
						{[1, 2, 4, 8, 16].map((b) => (
							<SelectItem key={b} value={`loop-${b}`} className="text-sm">
								Loop {b} bar{b > 1 ? "s" : ""}
							</SelectItem>
						))}
						<SelectItem value="trackTime" className="text-sm">
							Track time
						</SelectItem>
					</SelectContent>
				</Select>
			</Field>
			<Field label="Z-index">
				<Input
					type="number"
					value={zIndex}
					onChange={(e) => setZIndex(Number(e.target.value))}
					className="h-8 text-sm"
					min={0}
					max={99}
				/>
			</Field>
			<Field label="Blend mode">
				<Select value={blendMode} onValueChange={setBlendMode}>
					<SelectTrigger className="h-8 text-sm">
						<SelectValue />
					</SelectTrigger>
					<SelectContent>
						{[
							"replace",
							"add",
							"multiply",
							"screen",
							"max",
							"min",
							"lighten",
							"value",
							"subtract",
						].map((m) => (
							<SelectItem key={m} value={m} className="text-sm capitalize">
								{m}
							</SelectItem>
						))}
					</SelectContent>
				</Select>
			</Field>
		</div>
	);
}

// ─── action binding form ──────────────────────────────────────────────────────

export function CreateActionBindingForm({
	venueId,
	groups,
	modifiers,
	displayOrder,
	onCreated,
	onCancel,
}: {
	venueId: string;
	groups: FixtureGroup[];
	modifiers: ModifierDef[];
	displayOrder: number;
	onCreated: () => void;
	onCancel: () => void;
}) {
	const [trigger, setTrigger] = useState<MidiInput | null>(null);
	const [learning, setLearning] = useState(false);
	const [actionType, setActionType] = useState("setIntensity");
	const [intensityGroupId, setIntensityGroupId] = useState("");
	const [mode, setMode] = useState("tapToggleHoldFlash");
	const [requiredModifiers, setRequiredModifiers] = useState<string[]>([]);
	const [saving, setSaving] = useState(false);
	const [error, setError] = useState<string | null>(null);

	const isIntensity = actionType === "setIntensity";

	const startLearn = async () => {
		setLearning(true);
		try {
			await invoke("controller_start_learn");
			const unlisten = await listen<MidiInput>("midi_learn_captured", (e) => {
				let captured = e.payload;
				if (isIntensity && captured.type === "controlChange") {
					captured = {
						type: "controlChangeValue",
						channel: captured.channel,
						cc: captured.cc,
					};
				}
				setTrigger(captured);
				setLearning(false);
				unlisten();
			});
		} catch (e) {
			setError(String(e));
			setLearning(false);
		}
	};

	useEffect(() => {
		return () => {
			if (learning) invoke("controller_cancel_learn").catch(() => {});
		};
	}, [learning]);

	const toggleModifier = (name: string) => {
		setRequiredModifiers((prev) =>
			prev.includes(name) ? prev.filter((m) => m !== name) : [...prev, name],
		);
	};

	const create = async () => {
		if (!trigger) return;
		setSaving(true);
		setError(null);
		try {
			let action: object;
			if (isIntensity) {
				action = { type: "setIntensity", group_id: intensityGroupId || null };
			} else if (actionType === "blackout") {
				action = { type: "blackout" };
			} else {
				action = { type: "controllerActive" };
			}

			const triggerMode = isIntensity
				? null
				: mode === "toggle"
					? { type: "toggle" }
					: mode === "flash"
						? { type: "flash" }
						: { type: "tapToggleHoldFlash", threshold_ms: 300 };

			await invoke("midi_create_binding", {
				input: {
					venueId,
					trigger,
					requiredModifiers,
					exclusive: requiredModifiers.length > 0,
					mode: triggerMode,
					action,
					targetOverride: null,
					displayOrder,
				},
			});
			onCreated();
		} catch (e) {
			setError(String(e));
			setSaving(false);
		}
	};

	return (
		<FormCard>
			<Field label="Trigger">
				<div className="flex items-center gap-2">
					<div className="flex-1 border border-border/40 bg-background px-3 py-1.5 text-sm min-h-[32px] flex items-center">
						{learning ? (
							<span className="text-xs text-muted-foreground animate-pulse">
								Press a pad or knob…
							</span>
						) : trigger ? (
							<span className="text-xs font-mono">
								{labelMidiInput(trigger)}
							</span>
						) : (
							<span className="text-xs text-muted-foreground">Not set</span>
						)}
					</div>
					<Button
						variant="outline"
						size="sm"
						onClick={startLearn}
						disabled={learning}
					>
						{learning ? "Listening…" : "Learn"}
					</Button>
				</div>
			</Field>

			<div className="grid grid-cols-2 gap-3">
				<Field label="Action">
					<NativeSelect
						value={actionType}
						onChange={(e) => setActionType(e.target.value)}
						className="h-8 text-sm"
					>
						<NativeSelectOption value="setIntensity">
							Group brightness
						</NativeSelectOption>
						<NativeSelectOption value="blackout">Blackout</NativeSelectOption>
						<NativeSelectOption value="controllerActive">
							Controller on/off
						</NativeSelectOption>
					</NativeSelect>
				</Field>

				{!isIntensity && (
					<Field label="Mode">
						<NativeSelect
							value={mode}
							onChange={(e) => setMode(e.target.value)}
							className="h-8 text-sm"
						>
							<NativeSelectOption value="tapToggleHoldFlash">
								Tap toggle / hold flash
							</NativeSelectOption>
							<NativeSelectOption value="toggle">Toggle</NativeSelectOption>
							<NativeSelectOption value="flash">Flash</NativeSelectOption>
						</NativeSelect>
					</Field>
				)}
			</div>

			{isIntensity && (
				<Field label="Group">
					<NativeSelect
						value={intensityGroupId}
						onChange={(e) => setIntensityGroupId(e.target.value)}
						className="h-8 text-sm w-full"
					>
						<NativeSelectOption value="">Master (all)</NativeSelectOption>
						{groups.map((g) => (
							<NativeSelectOption key={g.id} value={g.id}>
								{g.name ?? g.id}
							</NativeSelectOption>
						))}
					</NativeSelect>
				</Field>
			)}

			{modifiers.length > 0 && !isIntensity && (
				<div className="space-y-1">
					<span className="text-xs text-muted-foreground">
						Only when holding
					</span>
					<div className="flex flex-wrap gap-2">
						{modifiers.map((m) => (
							<label
								key={m.id}
								className="flex items-center gap-1.5 cursor-pointer"
							>
								<input
									type="checkbox"
									checked={requiredModifiers.includes(m.name)}
									onChange={() => toggleModifier(m.name)}
									className="w-3 h-3"
								/>
								<span className="text-xs">{m.name}</span>
							</label>
						))}
					</div>
				</div>
			)}

			{error && <p className="text-xs text-destructive">{error}</p>}
			<FormActions
				onCancel={onCancel}
				onSubmit={create}
				disabled={saving || !trigger}
				label={saving ? "Creating…" : "Create"}
			/>
		</FormCard>
	);
}

// ─── modifier form ────────────────────────────────────────────────────────────

export function CreateModifierForm({
	venueId,
	groups,
	onCreated,
	onCancel,
}: {
	venueId: string;
	groups: FixtureGroup[];
	onCreated: () => void;
	onCancel: () => void;
}) {
	const [name, setName] = useState("");
	const [trigger, setTrigger] = useState<MidiInput | null>(null);
	const [learning, setLearning] = useState(false);
	const [selectedGroups, setSelectedGroups] = useState<string[]>([]);
	const [saving, setSaving] = useState(false);
	const [error, setError] = useState<string | null>(null);

	const startLearn = async () => {
		setLearning(true);
		try {
			await invoke("controller_start_learn");
			const unlisten = await listen<MidiInput>("midi_learn_captured", (e) => {
				setTrigger(e.payload);
				setLearning(false);
				unlisten();
			});
		} catch (e) {
			setError(String(e));
			setLearning(false);
		}
	};

	useEffect(() => {
		return () => {
			if (learning) invoke("controller_cancel_learn").catch(() => {});
		};
	}, [learning]);

	const toggleGroup = (id: string) => {
		setSelectedGroups((prev) =>
			prev.includes(id) ? prev.filter((g) => g !== id) : [...prev, id],
		);
	};

	const create = async () => {
		if (!name.trim() || !trigger) return;
		setSaving(true);
		setError(null);
		try {
			await invoke("midi_create_modifier", {
				input: {
					venueId,
					name: name.trim(),
					input: trigger,
					groups: selectedGroups.length > 0 ? selectedGroups : null,
				},
			});
			onCreated();
		} catch (e) {
			setError(String(e));
			setSaving(false);
		}
	};

	return (
		<FormCard>
			<div className="grid grid-cols-2 gap-3">
				<Field label="Name">
					<Input
						value={name}
						onChange={(e) => setName(e.target.value)}
						placeholder="e.g. A"
						className="h-8 text-sm"
						autoFocus
					/>
				</Field>
				<Field label="Button">
					<div className="flex items-center gap-2">
						<div className="flex-1 border border-border/40 bg-background px-3 py-1.5 text-sm min-h-[32px] flex items-center">
							{learning ? (
								<span className="text-xs text-muted-foreground animate-pulse">
									Hold a button…
								</span>
							) : trigger ? (
								<span className="text-xs font-mono">
									{labelMidiInput(trigger)}
								</span>
							) : (
								<span className="text-xs text-muted-foreground">Not set</span>
							)}
						</div>
						<Button
							variant="outline"
							size="sm"
							onClick={startLearn}
							disabled={learning}
						>
							Learn
						</Button>
					</div>
				</Field>
			</div>

			{groups.length > 0 && (
				<div className="space-y-1">
					<span className="text-xs text-muted-foreground">
						Target groups (optional)
					</span>
					<div className="flex flex-wrap gap-2 max-h-32 overflow-y-auto">
						{groups.map((g) => (
							<label
								key={g.id}
								className="flex items-center gap-1.5 cursor-pointer"
							>
								<input
									type="checkbox"
									checked={selectedGroups.includes(g.id)}
									onChange={() => toggleGroup(g.id)}
									className="w-3 h-3"
								/>
								<span className="text-xs">{g.name ?? g.id}</span>
							</label>
						))}
					</div>
				</div>
			)}

			{error && <p className="text-xs text-destructive">{error}</p>}
			<FormActions
				onCancel={onCancel}
				onSubmit={create}
				disabled={saving || !name.trim() || !trigger}
				label={saving ? "Creating…" : "Create"}
			/>
		</FormCard>
	);
}

// ─── shared primitives ────────────────────────────────────────────────────────

export function Empty({ children }: { children: React.ReactNode }) {
	return (
		<div className="border border-border/40 bg-background/50 p-4 text-center text-xs text-muted-foreground">
			{children}
		</div>
	);
}

export function Row({ children }: { children: React.ReactNode }) {
	return <div className="flex items-center gap-3 px-3 py-2.5">{children}</div>;
}

export function Field({
	label,
	children,
}: {
	label: string;
	children: React.ReactNode;
}) {
	return (
		<div className="space-y-1">
			<span className="text-xs text-muted-foreground">{label}</span>
			{children}
		</div>
	);
}

export function FormCard({ children }: { children: React.ReactNode }) {
	return (
		<div className="border border-border/40 bg-background/50 p-4 space-y-3">
			{children}
		</div>
	);
}

export function FormActions({
	onCancel,
	onSubmit,
	disabled,
	label,
}: {
	onCancel: () => void;
	onSubmit: () => void;
	disabled: boolean;
	label: string;
}) {
	return (
		<div className="flex gap-2 justify-end">
			<Button variant="ghost" size="sm" onClick={onCancel}>
				Cancel
			</Button>
			<Button size="sm" onClick={onSubmit} disabled={disabled}>
				{label}
			</Button>
		</div>
	);
}
