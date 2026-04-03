import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Loader2, Pencil, Plus, Settings2, Trash2, Zap } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import type { FixtureGroup } from "@/bindings/groups";
import type {
	ControllerState,
	ControllerStatus,
	Cue,
	MidiBinding,
	MidiInput,
	ModifierDef,
} from "@/bindings/midi";
import type { DeckState } from "@/bindings/perform";
import type { PatternArgDef, PatternSummary } from "@/bindings/schema";
import { useAppViewStore } from "@/features/app/stores/use-app-view-store";
import { useFixtureStore } from "@/features/universe/stores/use-fixture-store";
import { useGroupStore } from "@/features/universe/stores/use-group-store";
import { StageVisualizer } from "@/features/visualizer/components/stage-visualizer";
import { Button } from "@/shared/components/ui/button";
import {
	Dialog,
	DialogContent,
	DialogHeader,
	DialogTitle,
} from "@/shared/components/ui/dialog";
import {
	Select,
	SelectContent,
	SelectItem,
	SelectTrigger,
	SelectValue,
} from "@/shared/components/ui/select";
import { cn } from "@/shared/lib/utils";
import type { DeckMatchState } from "../stores/use-perform-store";
import { usePerformStore } from "../stores/use-perform-store";
import {
	argDefaultValue,
	CreateActionBindingForm,
	CreateModifierForm,
	CueArgsFields,
	CueBaseFields,
	DeviceSection,
	Empty,
	Field,
	labelAction,
	labelMidiInput,
	labelMode,
	Row,
} from "./controller-config";

const DECK_COLORS = ["#22d3ee", "#e879f9", "#4ade80", "#fb923c"];

// ─── main page ────────────────────────────────────────────────────────────────

export function PerformPage() {
	const connectionStatus = usePerformStore((s) => s.connectionStatus);
	const source = usePerformStore((s) => s.source);
	const deviceName = usePerformStore((s) => s.deviceName);
	const decks = usePerformStore((s) => s.decks);
	const crossfader = usePerformStore((s) => s.crossfader);
	const masterTempo = usePerformStore((s) => s.masterTempo);
	const error = usePerformStore((s) => s.error);
	const connect = usePerformStore((s) => s.connect);
	const disconnect = usePerformStore((s) => s.disconnect);
	const deckMatches = usePerformStore((s) => s.deckMatches);
	const activeDeckId = usePerformStore((s) => s.activeDeckId);
	const isCompositing = usePerformStore((s) => s.isCompositing);
	const currentVenue = useAppViewStore((s) => s.currentVenue);
	const currentVenueId = currentVenue?.id ?? null;

	const [cues, setCues] = useState<Cue[]>([]);
	const [bindings, setBindings] = useState<MidiBinding[]>([]);
	const [modifiers, setModifiers] = useState<ModifierDef[]>([]);
	const [patterns, setPatterns] = useState<PatternSummary[]>([]);
	const [flatGroups, setFlatGroups] = useState<FixtureGroup[]>([]);
	const [ctrlStatus, setCtrlStatus] = useState<ControllerStatus | null>(null);
	const [ctrlState, setCtrlState] = useState<ControllerState | null>(null);
	const groups = useGroupStore((s) => s.groups);
	const fetchGroups = useGroupStore((s) => s.fetchGroups);

	// controller panel state
	const [ctrlConfigOpen, setCtrlConfigOpen] = useState(false);
	const [ctrlTab, setCtrlTab] = useState<"cues" | "actions" | "modifiers">(
		"cues",
	);
	const [editMode, setEditMode] = useState(false);
	const [editingCue, setEditingCue] = useState<Cue | null>(null);
	const [creatingCue, setCreatingCue] = useState(false);

	// Initialize fixtures
	useEffect(() => {
		if (currentVenueId !== null) {
			useFixtureStore.getState().initialize(currentVenueId);
		} else {
			useFixtureStore.getState().initialize();
		}
	}, [currentVenueId]);

	// Init controller + compile sim deck
	useEffect(() => {
		if (currentVenueId === null) return;
		invoke("controller_init_for_venue", {
			controllerPort: currentVenue?.controllerPort ?? null,
		}).catch(() => {});
		invoke("midi_reload_mapping", { venueId: currentVenueId }).catch(() => {});
		fetchGroups(currentVenueId);
	}, [currentVenueId, currentVenue?.controllerPort, fetchGroups]);

	const reloadData = async () => {
		if (!currentVenueId) return;
		try {
			const [c, b, m, p, g] = await Promise.all([
				invoke<Cue[]>("midi_list_cues", { venueId: currentVenueId }),
				invoke<MidiBinding[]>("midi_list_bindings", {
					venueId: currentVenueId,
				}),
				invoke<ModifierDef[]>("midi_list_modifiers", {
					venueId: currentVenueId,
				}),
				invoke<PatternSummary[]>("list_patterns"),
				invoke<FixtureGroup[]>("list_groups", { venueId: currentVenueId }),
			]);
			setCues(c);
			setBindings(b);
			setModifiers(m);
			setPatterns(p);
			setFlatGroups(g);
		} catch {}
	};

	useEffect(() => {
		reloadData();
	}, [currentVenueId]);

	// Poll controller status every 2s
	useEffect(() => {
		const pollStatus = async () => {
			try {
				setCtrlStatus(await invoke<ControllerStatus>("controller_get_status"));
			} catch {}
		};
		pollStatus();
		const id = setInterval(pollStatus, 2000);
		return () => clearInterval(id);
	}, []);

	// Real-time controller state
	useEffect(() => {
		let unlisten: (() => void) | null = null;
		invoke<ControllerState>("controller_get_state")
			.then(setCtrlState)
			.catch(() => {});
		listen<ControllerState>("controller_state", (e) => {
			setCtrlState(e.payload);
		}).then((fn) => {
			unlisten = fn;
		});
		return () => {
			unlisten?.();
		};
	}, []);

	// Cleanup on unmount
	useEffect(() => {
		return () => {
			invoke("render_clear_perform").catch(() => {});
			const { connectionStatus } = usePerformStore.getState();
			if (
				connectionStatus === "connected" ||
				connectionStatus === "connecting"
			) {
				usePerformStore.getState().disconnect();
			}
		};
	}, []);

	const activeDeck = activeDeckId ? decks.get(activeDeckId) : null;
	const activeMatch = activeDeckId ? deckMatches.get(activeDeckId) : null;
	const renderAudioTimeSec = useMemo(() => {
		if (activeMatch?.hasLightShow && activeDeck && activeDeck.sample_rate > 0) {
			return activeDeck.samples / activeDeck.sample_rate;
		}
		return null;
	}, [activeDeck, activeMatch?.hasLightShow]);

	const toggleCue = async (cueId: string) => {
		const isActive = ctrlState?.activeCueIds.includes(cueId) ?? false;
		try {
			if (isActive) {
				await invoke("midi_release_cue", { cueId });
			} else {
				await invoke("midi_fire_cue", { cueId });
			}
		} catch {}
	};

	const toggleControllerActive = async () => {
		try {
			await invoke("controller_set_active", { active: !ctrlState?.active });
		} catch {}
	};

	const reorderCues = async (reordered: Cue[]) => {
		setCues(reordered);
		// Persist new display_order for each cue
		await Promise.all(
			reordered.map((cue, i) =>
				invoke("midi_update_cue", {
					input: { id: cue.id, displayOrder: i },
				}).catch(() => {}),
			),
		);
	};

	const deckArray = Array.from(decks.values()).slice(0, 3);
	const displayBpm = masterTempo > 0 ? masterTempo.toFixed(1) : "—";

	// Intensity bindings for fader display
	const intensityBindings = bindings.filter(
		(b) => b.action.type === "setIntensity",
	) as (MidiBinding & {
		action: { type: "setIntensity"; group_id: string | null };
	})[];

	return (
		<div className="flex flex-col h-full bg-background font-mono select-none">
			{/* ── top bar ── */}
			<TopBar
				bpm={displayBpm}
				isCompositing={isCompositing}
				ctrlStatus={ctrlStatus}
				ctrlState={ctrlState}
				onToggleActive={toggleControllerActive}
			/>

			{/* ── main area ── */}
			<div className="flex flex-1 min-h-0 border-t border-border/30">
				{/* left: visualizer */}
				<div
					className="relative border-r border-border/30"
					style={{ width: "58%" }}
				>
					<StageVisualizer
						enableEditing={false}
						renderAudioTimeSec={renderAudioTimeSec}
					/>
				</div>

				{/* right: decks + controller */}
				<div className="flex flex-col" style={{ width: "42%" }}>
					{/* deck panel */}
					<DeckPanel
						connectionStatus={connectionStatus}
						deviceName={deviceName}
						error={error}
						decks={deckArray}
						deckMatches={deckMatches}
						activeDeckId={activeDeckId}
						crossfader={crossfader}
						onConnect={connect}
						onDisconnect={disconnect}
					/>

					{/* controller panel */}
					<div className="flex-1 min-h-0 border-t border-border/30 flex flex-col">
						{/* panel header */}
						<div className="shrink-0 border-b border-border/20">
							{/* title row */}
							<div className="flex items-center justify-between px-3 h-7">
								<div className="flex items-center gap-2">
									<span className="text-[10px] tracking-widest text-muted-foreground uppercase">
										Controller
									</span>
									{ctrlState?.heldModifiers &&
										ctrlState.heldModifiers.length > 0 && (
											<div className="flex gap-1">
												{ctrlState.heldModifiers.map((m) => (
													<span
														key={m}
														className="text-[10px] bg-amber-400/20 text-amber-400 px-1"
													>
														{m}
													</span>
												))}
											</div>
										)}
								</div>
								<button
									type="button"
									onClick={() => setCtrlConfigOpen(true)}
									className="flex items-center gap-1 text-[10px] text-muted-foreground/70 hover:text-muted-foreground transition-colors"
								>
									<Settings2 className="w-3 h-3" />
									configure
								</button>
							</div>

							{/* intensity faders */}
							{intensityBindings.length > 0 && (
								<div className="flex items-end gap-2 px-3 pb-2 pt-1">
									{intensityBindings
										.filter((b) => b.action.group_id === null)
										.map((b) => (
											<FaderBar
												key={b.id}
												label="MASTER"
												value={ctrlState?.masterIntensity ?? 1}
											/>
										))}
									{intensityBindings
										.filter((b) => b.action.group_id !== null)
										.map((b) => {
											const gid = b.action.group_id as string;
											const groupName =
												groups.find((g) => g.groupId === gid)?.groupName ??
												gid.slice(0, 6);
											const value = ctrlState?.groupIntensities?.[gid] ?? 1;
											return (
												<FaderBar
													key={b.id}
													label={groupName?.toUpperCase() ?? "—"}
													value={value}
												/>
											);
										})}
								</div>
							)}

							{/* tab bar */}
							<div className="flex items-center border-t border-border/20">
								{(["cues", "actions", "modifiers"] as const).map((t) => (
									<button
										key={t}
										type="button"
										onClick={() => {
											setCtrlTab(t);
											setEditMode(false);
										}}
										className={cn(
											"px-3 py-1 text-[10px] tracking-widest uppercase transition-colors border-b-2 -mb-px",
											ctrlTab === t
												? "border-foreground/60 text-foreground"
												: "border-transparent text-muted-foreground/60 hover:text-muted-foreground",
										)}
									>
										{t}
									</button>
								))}
								{ctrlTab === "cues" && (
									<button
										type="button"
										onClick={() => setEditMode((v) => !v)}
										className={cn(
											"ml-auto mr-2 flex items-center gap-1 px-2 py-0.5 text-[10px] tracking-wider border transition-colors",
											editMode
												? "border-orange-400/50 text-orange-400 bg-orange-400/10"
												: "border-border/30 text-muted-foreground/60 hover:text-muted-foreground",
										)}
									>
										<Pencil className="w-2.5 h-2.5" />
										EDIT
									</button>
								)}
							</div>
						</div>

						{/* tab content */}
						<div className="flex-1 min-h-0 overflow-y-auto p-3">
							{ctrlTab === "cues" && (
								<PadGrid
									cues={cues}
									ctrlState={ctrlState}
									editMode={editMode}
									onToggle={toggleCue}
									onEditCue={setEditingCue}
									onCreateCue={() => setCreatingCue(true)}
									onReorder={reorderCues}
								/>
							)}
							{ctrlTab === "actions" && currentVenueId && (
								<ActionsSection
									venueId={currentVenueId}
									bindings={bindings}
									cues={cues}
									groups={flatGroups}
									modifiers={modifiers}
									onChange={() => {
										reloadData();
										invoke("midi_reload_mapping", {
											venueId: currentVenueId,
										}).catch(() => {});
									}}
								/>
							)}
							{ctrlTab === "modifiers" && currentVenueId && (
								<ModifiersSection
									venueId={currentVenueId}
									modifiers={modifiers}
									groups={flatGroups}
									onChange={() => {
										reloadData();
										invoke("midi_reload_mapping", {
											venueId: currentVenueId,
										}).catch(() => {});
									}}
								/>
							)}
						</div>
					</div>
				</div>
			</div>

			{/* ── bottom bar ── */}
			<BottomBar connectionStatus={connectionStatus} source={source} />

			{/* ── configure controller dialog ── */}
			{currentVenueId && (
				<ConfigureControllerDialog
					open={ctrlConfigOpen}
					onOpenChange={setCtrlConfigOpen}
				/>
			)}

			{/* ── cue editor dialog ── */}
			{(editingCue || creatingCue) && currentVenueId && (
				<CueEditorDialog
					cue={editingCue}
					isOpen={true}
					onClose={() => {
						setEditingCue(null);
						setCreatingCue(false);
					}}
					venueId={currentVenueId}
					patterns={patterns}
					existingBinding={
						editingCue
							? (bindings.find(
									(b) =>
										b.action.type === "fireCue" &&
										(b.action as { type: "fireCue"; cue_id: string }).cue_id ===
											editingCue.id &&
										b.requiredModifiers.length === 0,
								) ?? null)
							: null
					}
					cueCount={cues.length}
					onSaved={() => {
						setEditingCue(null);
						setCreatingCue(false);
						reloadData();
						if (currentVenueId) {
							invoke("midi_reload_mapping", { venueId: currentVenueId }).catch(
								() => {},
							);
						}
					}}
					onDeleted={() => {
						setEditingCue(null);
						reloadData();
						if (currentVenueId) {
							invoke("midi_reload_mapping", { venueId: currentVenueId }).catch(
								() => {},
							);
						}
					}}
				/>
			)}
		</div>
	);
}

// ─── configure controller dialog ──────────────────────────────────────────────

function ConfigureControllerDialog({
	open,
	onOpenChange,
}: {
	open: boolean;
	onOpenChange: (v: boolean) => void;
}) {
	const [status, setCtrlStatus] = useState<ControllerStatus | null>(null);
	const [connectError, setConnectError] = useState<string | null>(null);
	const [connecting, setConnecting] = useState(false);

	const refreshStatus = async () => {
		try {
			setCtrlStatus(await invoke<ControllerStatus>("controller_get_status"));
			setConnectError(null);
		} catch (e) {
			setConnectError(String(e));
		}
	};

	useEffect(() => {
		if (!open) return;
		refreshStatus();
		const id = setInterval(refreshStatus, 2000);
		return () => clearInterval(id);
	}, [open]);

	const handleConnect = async (portName: string) => {
		setConnecting(true);
		setConnectError(null);
		try {
			const currentVenue = useAppViewStore.getState().currentVenue;
			await invoke("controller_connect", {
				portName,
				venueId: currentVenue?.id ?? "",
			});
			await refreshStatus();
		} catch (e) {
			setConnectError(String(e));
		} finally {
			setConnecting(false);
		}
	};

	const handleDisconnect = async () => {
		try {
			const currentVenue = useAppViewStore.getState().currentVenue;
			await invoke("controller_disconnect", {
				venueId: currentVenue?.id ?? "",
			});
			await refreshStatus();
		} catch (e) {
			setConnectError(String(e));
		}
	};

	return (
		<Dialog open={open} onOpenChange={onOpenChange}>
			<DialogContent className="max-w-sm">
				<DialogHeader>
					<DialogTitle className="text-sm font-medium">
						Configure Controller
					</DialogTitle>
				</DialogHeader>
				<DeviceSection
					status={status}
					connecting={connecting}
					error={connectError}
					onConnect={handleConnect}
					onDisconnect={handleDisconnect}
					onRefresh={refreshStatus}
				/>
			</DialogContent>
		</Dialog>
	);
}

// ─── cue editor dialog ────────────────────────────────────────────────────────

function CueEditorDialog({
	cue,
	isOpen,
	onClose,
	venueId,
	patterns,
	existingBinding,
	cueCount,
	onSaved,
	onDeleted,
}: {
	cue: Cue | null;
	isOpen: boolean;
	onClose: () => void;
	venueId: string;
	patterns: PatternSummary[];
	existingBinding: MidiBinding | null;
	cueCount: number;
	onSaved: () => void;
	onDeleted?: () => void;
}) {
	const isEdit = cue !== null;

	// derive initial exec mode string
	const initExec = () => {
		if (!cue) return "loop-4";
		if (cue.executionMode.type === "loop")
			return `loop-${cue.executionMode.bars}`;
		return "trackTime";
	};

	const [name, setName] = useState(cue?.name ?? "");
	const [patternId, setPatternId] = useState(
		cue?.patternId ?? patterns[0]?.id ?? "",
	);
	const [execMode, setExecMode] = useState(initExec);
	const [zIndex, setZIndex] = useState(cue?.zIndex ?? 1);
	const [blendMode, setBlendMode] = useState(() => {
		if (!cue) return "replace";
		return typeof cue.blendMode === "string"
			? (cue.blendMode as string).toLowerCase()
			: "replace";
	});
	const [patternArgs, setPatternArgs] = useState<PatternArgDef[]>([]);
	const [argValues, setArgValues] = useState<Record<string, unknown>>(
		cue ? (cue.args as Record<string, unknown>) : {},
	);

	// Binding state
	const [bindingTrigger, setBindingTrigger] = useState<MidiInput | null>(
		existingBinding?.trigger ?? null,
	);
	const [bindingMode, setBindingMode] = useState<string>(() => {
		if (!existingBinding) return "tapToggleHoldFlash";
		const m = existingBinding.mode;
		if (m.type === "toggle") return "toggle";
		if (m.type === "flash") return "flash";
		return "tapToggleHoldFlash";
	});
	const [learning, setLearning] = useState(false);
	const [saving, setSaving] = useState(false);
	const [error, setError] = useState<string | null>(null);
	const lastPatternRef = useRef<string | null>(null);

	// Fetch pattern args when pattern changes
	useEffect(() => {
		if (!patternId || patternId === lastPatternRef.current) return;
		lastPatternRef.current = patternId;
		invoke<PatternArgDef[]>("get_pattern_args", { id: patternId })
			.then((args) => {
				setPatternArgs(args);
				setArgValues((prev) => {
					const next = { ...prev };
					for (const arg of args) {
						if (!(arg.id in next)) {
							next[arg.id] = argDefaultValue(arg);
						}
					}
					return next;
				});
			})
			.catch(() => setPatternArgs([]));
	}, [patternId]);

	const startLearn = async () => {
		setLearning(true);
		try {
			await invoke("controller_start_learn");
			const unlisten = await listen<MidiInput>("midi_learn_captured", (e) => {
				setBindingTrigger(e.payload);
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

	const setArgValue = (id: string, value: unknown) => {
		setArgValues((prev) => ({ ...prev, [id]: value }));
	};

	const save = async () => {
		if (!name.trim() || !patternId) return;
		setSaving(true);
		setError(null);
		try {
			const [execType, barsStr] = execMode.split("-");
			const executionMode =
				execType === "loop"
					? { type: "loop", bars: Number(barsStr) }
					: { type: "trackTime" };

			let savedCueId: string;

			if (isEdit) {
				await invoke("midi_update_cue", {
					input: {
						id: cue.id,
						name: name.trim(),
						patternId,
						args: Object.keys(argValues).length > 0 ? argValues : undefined,
						zIndex,
						blendMode,
						executionMode,
					},
				});
				savedCueId = cue.id;
			} else {
				const created = await invoke<Cue>("midi_create_cue", {
					input: {
						venueId,
						name: name.trim(),
						patternId,
						args: Object.keys(argValues).length > 0 ? argValues : undefined,
						zIndex,
						blendMode,
						executionMode,
						displayOrder: cueCount,
					},
				});
				savedCueId = created.id;
			}

			// Save binding if trigger is set
			if (bindingTrigger) {
				const triggerMode =
					bindingMode === "toggle"
						? { type: "toggle" }
						: bindingMode === "flash"
							? { type: "flash" }
							: { type: "tapToggleHoldFlash", threshold_ms: 300 };

				if (existingBinding && isEdit) {
					await invoke("midi_update_binding", {
						input: {
							id: existingBinding.id,
							trigger: bindingTrigger,
							mode: triggerMode,
						},
					});
				} else {
					await invoke("midi_create_binding", {
						input: {
							venueId,
							trigger: bindingTrigger,
							requiredModifiers: [],
							exclusive: false,
							mode: triggerMode,
							action: { type: "fireCue", cue_id: savedCueId },
							targetOverride: null,
							displayOrder: 0,
						},
					});
				}
			}

			onSaved();
		} catch (e) {
			setError(String(e));
			setSaving(false);
		}
	};

	const deleteCue = async () => {
		if (!cue) return;
		try {
			await invoke("midi_delete_cue", { id: cue.id });
			onDeleted?.();
		} catch (e) {
			setError(String(e));
		}
	};

	return (
		<Dialog
			open={isOpen}
			onOpenChange={(v) => {
				if (!v) onClose();
			}}
		>
			<DialogContent className="max-w-md max-h-[85vh] overflow-y-auto">
				<DialogHeader>
					<DialogTitle className="text-sm font-medium">
						{isEdit ? "Edit Cue" : "New Cue"}
					</DialogTitle>
				</DialogHeader>

				<div className="space-y-4">
					<CueBaseFields
						name={name}
						setName={setName}
						patternId={patternId}
						setPatternId={setPatternId}
						execMode={execMode}
						setExecMode={setExecMode}
						zIndex={zIndex}
						setZIndex={setZIndex}
						blendMode={blendMode}
						setBlendMode={setBlendMode}
						patterns={patterns}
					/>

					<CueArgsFields
						patternArgs={patternArgs}
						argValues={argValues}
						setArgValue={setArgValue}
					/>

					{/* MIDI trigger section */}
					<div className="space-y-2 pt-1 border-t border-border/40">
						<span className="text-xs text-muted-foreground">MIDI Trigger</span>
						<div className="grid grid-cols-2 gap-3">
							<Field label="Trigger">
								<div className="flex items-center gap-2">
									<div className="flex-1 border border-border/40 bg-background px-2 py-1.5 text-sm min-h-[32px] flex items-center">
										{learning ? (
											<span className="text-xs text-muted-foreground animate-pulse">
												Press a pad…
											</span>
										) : bindingTrigger ? (
											<span className="text-xs font-mono">
												{labelMidiInput(bindingTrigger)}
											</span>
										) : (
											<span className="text-xs text-muted-foreground">
												None
											</span>
										)}
									</div>
									<Button
										variant="outline"
										size="sm"
										onClick={startLearn}
										disabled={learning}
									>
										{learning ? "…" : "Learn"}
									</Button>
								</div>
							</Field>
							<Field label="Mode">
								<Select value={bindingMode} onValueChange={setBindingMode}>
									<SelectTrigger className="h-8 text-sm">
										<SelectValue />
									</SelectTrigger>
									<SelectContent>
										<SelectItem value="tapToggleHoldFlash" className="text-sm">
											Tap/hold flash
										</SelectItem>
										<SelectItem value="toggle" className="text-sm">
											Toggle
										</SelectItem>
										<SelectItem value="flash" className="text-sm">
											Flash
										</SelectItem>
									</SelectContent>
								</Select>
							</Field>
						</div>
					</div>

					{error && <p className="text-xs text-destructive">{error}</p>}

					<div className="flex items-center gap-2 pt-1">
						{isEdit && (
							<button
								type="button"
								onClick={deleteCue}
								className="flex items-center gap-1 text-xs text-destructive/70 hover:text-destructive transition-colors"
							>
								<Trash2 className="w-3 h-3" />
								Delete
							</button>
						)}
						<div className="flex-1" />
						<Button variant="ghost" size="sm" onClick={onClose}>
							Cancel
						</Button>
						<Button
							size="sm"
							onClick={save}
							disabled={saving || !name.trim() || !patternId}
						>
							{saving ? "Saving…" : "Save"}
						</Button>
					</div>
				</div>
			</DialogContent>
		</Dialog>
	);
}

// ─── actions section ──────────────────────────────────────────────────────────

function ActionsSection({
	venueId,
	bindings,
	cues,
	groups,
	modifiers,
	onChange,
}: {
	venueId: string;
	bindings: MidiBinding[];
	cues: Cue[];
	groups: FixtureGroup[];
	modifiers: ModifierDef[];
	onChange: () => void;
}) {
	const [adding, setAdding] = useState(false);
	const [confirmDelete, setConfirmDelete] = useState<string | null>(null);

	// Only show non-fireCue bindings
	const actionBindings = bindings.filter((b) => b.action.type !== "fireCue");

	const deleteBinding = async (id: string) => {
		await invoke("midi_delete_binding", { id });
		setConfirmDelete(null);
		onChange();
	};

	return (
		<div className="space-y-3">
			{actionBindings.length === 0 && !adding ? (
				<Empty>No action bindings yet</Empty>
			) : (
				<div className="border border-border/40 divide-y divide-border/40">
					{actionBindings.map((b) => (
						<Row key={b.id}>
							<div className="flex-1 min-w-0 space-y-0.5">
								<div className="flex items-center gap-1.5 flex-wrap">
									<span className="text-xs font-mono bg-muted px-1.5 py-0.5 rounded">
										{labelMidiInput(b.trigger)}
									</span>
									<span className="text-xs text-muted-foreground">→</span>
									<span className="text-xs">
										{labelAction(b.action, cues, groups)}
									</span>
								</div>
								{b.action.type !== "setIntensity" && (
									<div className="text-xs text-muted-foreground">
										{labelMode(b.mode)}
									</div>
								)}
							</div>
							<DeleteConfirm
								id={b.id}
								active={confirmDelete}
								onRequest={setConfirmDelete}
								onConfirm={deleteBinding}
							/>
						</Row>
					))}
				</div>
			)}

			{adding ? (
				<CreateActionBindingForm
					venueId={venueId}
					groups={groups}
					modifiers={modifiers}
					displayOrder={bindings.length}
					onCreated={() => {
						setAdding(false);
						onChange();
					}}
					onCancel={() => setAdding(false)}
				/>
			) : (
				<button
					type="button"
					onClick={() => setAdding(true)}
					className="flex items-center gap-1.5 w-full text-xs text-muted-foreground/70 hover:text-muted-foreground transition-colors py-1"
				>
					<Plus className="w-3 h-3" />
					Add action binding
				</button>
			)}
		</div>
	);
}

// ─── modifiers section ────────────────────────────────────────────────────────

function ModifiersSection({
	venueId,
	modifiers,
	groups,
	onChange,
}: {
	venueId: string;
	modifiers: ModifierDef[];
	groups: FixtureGroup[];
	onChange: () => void;
}) {
	const [adding, setAdding] = useState(false);
	const [confirmDelete, setConfirmDelete] = useState<string | null>(null);

	const deleteModifier = async (id: string) => {
		await invoke("midi_delete_modifier", { id });
		setConfirmDelete(null);
		onChange();
	};

	return (
		<div className="space-y-3">
			<p className="text-xs text-muted-foreground/70">
				Hold a modifier while pressing a pad to target specific fixture groups.
			</p>

			{modifiers.length === 0 && !adding ? (
				<Empty>No modifiers yet</Empty>
			) : (
				<div className="border border-border/40 divide-y divide-border/40">
					{modifiers.map((m) => {
						const groupNames = m.groups
							?.map((gid) => groups.find((g) => g.id === gid)?.name ?? gid)
							.join(", ");
						return (
							<Row key={m.id}>
								<div className="flex-1 min-w-0">
									<div className="flex items-center gap-2">
										<span className="text-sm font-medium">{m.name}</span>
										<span className="text-xs font-mono bg-muted px-1.5 py-0.5 rounded">
											{labelMidiInput(m.input)}
										</span>
									</div>
									{groupNames && (
										<div className="text-xs text-muted-foreground truncate mt-0.5">
											→ {groupNames}
										</div>
									)}
								</div>
								<DeleteConfirm
									id={m.id}
									active={confirmDelete}
									onRequest={setConfirmDelete}
									onConfirm={deleteModifier}
								/>
							</Row>
						);
					})}
				</div>
			)}

			{adding ? (
				<CreateModifierForm
					venueId={venueId}
					groups={groups}
					onCreated={() => {
						setAdding(false);
						onChange();
					}}
					onCancel={() => setAdding(false)}
				/>
			) : (
				<button
					type="button"
					onClick={() => setAdding(true)}
					className="flex items-center gap-1.5 w-full text-xs text-muted-foreground/70 hover:text-muted-foreground transition-colors py-1"
				>
					<Plus className="w-3 h-3" />
					Add modifier
				</button>
			)}
		</div>
	);
}

// ─── top bar ──────────────────────────────────────────────────────────────────

function TopBar({
	bpm,
	isCompositing,
	ctrlStatus,
	ctrlState,
	onToggleActive,
}: {
	bpm: string;
	isCompositing: boolean;
	ctrlStatus: ControllerStatus | null;
	ctrlState: ControllerState | null;
	onToggleActive: () => void;
}) {
	return (
		<div className="flex items-center justify-between px-4 h-9 shrink-0 border-b border-border/30">
			<div className="flex items-center gap-3">
				<span className="text-xs font-bold tracking-widest text-orange-400">
					LUMA
				</span>
				<span className="text-[10px] tracking-widest text-muted-foreground uppercase">
					Live Performance
				</span>
			</div>

			<div className="flex items-center gap-1">
				<span className="text-lg font-bold tabular-nums text-foreground">
					{bpm}
				</span>
				<span className="text-[10px] text-muted-foreground ml-0.5">BPM</span>
			</div>

			<div className="flex items-center gap-3">
				{isCompositing && (
					<Loader2 className="w-3 h-3 animate-spin text-muted-foreground" />
				)}
				<button
					type="button"
					onClick={onToggleActive}
					title={
						ctrlState?.active ? "Controller output on" : "Controller output off"
					}
					className={cn(
						"flex items-center gap-1 px-2 py-0.5 text-[10px] tracking-wider border transition-colors",
						ctrlState?.active
							? "border-orange-400/50 text-orange-400 bg-orange-400/10"
							: "border-border/30 text-muted-foreground/70 hover:text-muted-foreground",
					)}
				>
					<Zap className="w-2.5 h-2.5" />
					OUTPUT
				</button>
				{ctrlStatus && (
					<div className="flex items-center gap-1.5">
						<div
							className={cn(
								"h-1.5 w-1.5 rounded-full",
								ctrlStatus.connected
									? "bg-green-500"
									: "bg-muted-foreground/30",
							)}
						/>
						<span className="text-[10px] text-muted-foreground truncate max-w-32">
							{ctrlStatus.connected && ctrlStatus.portName
								? ctrlStatus.portName
								: "No controller"}
						</span>
					</div>
				)}
			</div>
		</div>
	);
}

// ─── deck panel ───────────────────────────────────────────────────────────────

function DeckPanel({
	connectionStatus,
	deviceName,
	error,
	decks,
	deckMatches,
	activeDeckId,
	crossfader,
	onConnect,
	onDisconnect,
}: {
	connectionStatus: string;
	deviceName: string | null;
	error: string | null;
	decks: DeckState[];
	deckMatches: Map<number, DeckMatchState>;
	activeDeckId: number | null;
	crossfader: number;
	onConnect: (source: "stagelinq") => void;
	onDisconnect: () => void;
}) {
	const [showSourceMenu, setShowSourceMenu] = useState(false);
	const menuRef = useRef<HTMLDivElement>(null);

	useEffect(() => {
		if (!showSourceMenu) return;
		const handler = (e: MouseEvent) => {
			if (!menuRef.current?.contains(e.target as Node))
				setShowSourceMenu(false);
		};
		document.addEventListener("mousedown", handler);
		return () => document.removeEventListener("mousedown", handler);
	}, [showSourceMenu]);

	const isConnected = connectionStatus === "connected";
	const isConnecting = connectionStatus === "connecting";

	return (
		<div
			className="flex flex-col border-b border-border/30"
			style={{ minHeight: 0 }}
		>
			{/* header */}
			<div className="flex items-center justify-between px-3 h-7 shrink-0 border-b border-border/20">
				<span className="text-[10px] tracking-widest text-muted-foreground uppercase">
					Decks
				</span>
				<div className="relative" ref={menuRef}>
					{isConnected ? (
						<div className="flex items-center gap-2">
							{deviceName && (
								<span className="text-[10px] text-muted-foreground truncate max-w-28">
									{deviceName}
								</span>
							)}
							<button
								type="button"
								onClick={onDisconnect}
								className="text-[10px] text-muted-foreground/70 hover:text-muted-foreground transition-colors"
							>
								disconnect
							</button>
						</div>
					) : isConnecting ? (
						<div className="flex items-center gap-2">
							<span className="text-[10px] text-muted-foreground animate-pulse">
								searching…
							</span>
							<button
								type="button"
								onClick={onDisconnect}
								className="text-[10px] text-muted-foreground/70 hover:text-muted-foreground transition-colors"
							>
								cancel
							</button>
						</div>
					) : (
						<button
							type="button"
							onClick={() => setShowSourceMenu((v) => !v)}
							className="text-[10px] text-muted-foreground/70 hover:text-muted-foreground transition-colors"
						>
							+ connect source
						</button>
					)}

					{showSourceMenu && (
						<div className="absolute right-0 top-full mt-1 z-50 bg-popover border border-border/40 py-1 min-w-36">
							<button
								type="button"
								className="w-full px-3 py-1.5 text-left text-xs text-muted-foreground hover:text-foreground hover:bg-muted/30 transition-colors"
								onClick={() => {
									onConnect("stagelinq");
									setShowSourceMenu(false);
								}}
							>
								StageLinQ (Denon)
							</button>
							<button
								type="button"
								className="w-full px-3 py-1.5 text-left text-xs text-muted-foreground/60 cursor-not-allowed"
								disabled
							>
								Pro DJ Link (soon)
							</button>
						</div>
					)}
				</div>
			</div>

			{/* deck strips */}
			<div className="flex flex-col">
				{error && (
					<div className="px-3 py-1.5 text-xs text-destructive/80 border-b border-border/20">
						{error} ·{" "}
						<button
							type="button"
							className="underline"
							onClick={() => onConnect("stagelinq")}
						>
							retry
						</button>
					</div>
				)}
				{decks.length > 0 ? (
					decks.map((deck) => (
						<CompactDeckStrip
							key={deck.id}
							deck={deck}
							matchState={deckMatches.get(deck.id)}
							isActive={activeDeckId === deck.id}
						/>
					))
				) : isConnecting ? (
					<div className="px-3 py-4 text-center text-[10px] tracking-wider text-muted-foreground/50 uppercase">
						Searching for devices…
					</div>
				) : (
					<SimDeckStrip />
				)}
			</div>

			{/* crossfader */}
			{decks.length > 0 && (
				<div className="flex items-center gap-2 px-3 py-1.5 border-t border-border/20">
					<span className="text-[10px] tracking-wider text-muted-foreground/70 uppercase w-12">
						Xfader
					</span>
					<div className="h-px bg-muted-foreground/10 flex-1 relative">
						<div
							className="absolute top-1/2 -translate-y-1/2 w-1.5 h-2.5 bg-foreground/40"
							style={{ left: `${(crossfader * 100).toFixed(0)}%` }}
						/>
					</div>
				</div>
			)}
		</div>
	);
}

function CompactDeckStrip({
	deck,
	matchState,
	isActive,
}: {
	deck: DeckState;
	matchState?: DeckMatchState;
	isActive: boolean;
}) {
	const colorIndex = (deck.id - 1) % DECK_COLORS.length;
	const color = DECK_COLORS[colorIndex];
	const bpm = deck.beat_bpm > 0 ? deck.beat_bpm : deck.bpm;
	const beatInBar = deck.beat > 0 ? (Math.floor(deck.beat) % 4) + 1 : 0;
	const progress =
		deck.total_beats > 0 ? (deck.beat / deck.total_beats) * 100 : 0;

	return (
		<div
			className={cn(
				"px-3 py-2 border-b border-border/20 transition-colors",
				isActive ? "bg-muted/10" : "",
			)}
		>
			<div className="flex items-center justify-between mb-1">
				<div className="flex items-center gap-2">
					<span
						className="text-[10px] font-bold tracking-widest"
						style={{ color }}
					>
						DECK {deck.id}
					</span>
					<div
						className={cn(
							"h-1.5 w-1.5 rounded-full",
							deck.playing ? "bg-green-500" : "bg-muted-foreground/20",
						)}
					/>
					{matchState?.hasLightShow && (
						<span className="text-[10px] text-amber-400 tracking-wider">
							SHOW
						</span>
					)}
					{matchState?.matching && (
						<span className="text-[10px] text-muted-foreground animate-pulse">
							matching
						</span>
					)}
				</div>
				<div className="flex items-baseline gap-1">
					<span className="text-sm font-bold tabular-nums text-foreground/90">
						{bpm > 0 ? bpm.toFixed(1) : "---"}
					</span>
					<span className="text-[10px] text-muted-foreground/70">BPM</span>
				</div>
			</div>

			<div className="text-xs text-muted-foreground/70 truncate mb-1.5">
				{deck.title || (deck.song_loaded ? "Unknown Track" : "—")}
			</div>

			<div className="flex items-center gap-2">
				<div className="flex gap-0.5">
					{[1, 2, 3, 4].map((b) => (
						<div
							key={b}
							className="w-2 h-2"
							style={{
								backgroundColor:
									Math.ceil(beatInBar) === b ? color : "rgba(255,255,255,0.08)",
							}}
						/>
					))}
				</div>
				<div className="h-px flex-1 bg-muted-foreground/10">
					<div
						className="h-full transition-[width] duration-75"
						style={{
							width: `${Math.min(progress, 100)}%`,
							backgroundColor: color,
							opacity: 0.4,
						}}
					/>
				</div>
			</div>
		</div>
	);
}

function SimDeckStrip() {
	return (
		<div className="px-3 py-2 border-b border-border/20">
			<div className="flex items-center justify-between mb-1">
				<div className="flex items-center gap-2">
					<span className="text-[10px] font-bold tracking-widest text-muted-foreground/60">
						SIM
					</span>
					<div className="h-1.5 w-1.5 rounded-full bg-green-500/60" />
				</div>
				<div className="flex items-baseline gap-1">
					<span className="text-sm font-bold tabular-nums text-foreground/50">
						120.0
					</span>
					<span className="text-[10px] text-muted-foreground/50">BPM</span>
				</div>
			</div>
			<div className="text-xs text-muted-foreground/40 truncate mb-1.5">
				Virtual deck — 30s loop
			</div>
			<div className="h-px flex-1 bg-muted-foreground/5" />
		</div>
	);
}

// ─── fader bar ────────────────────────────────────────────────────────────────

function FaderBar({ label, value }: { label: string; value: number }) {
	const pct = Math.round(value * 100);
	return (
		<div className="flex flex-col items-center gap-1 w-8">
			<div className="relative w-2 h-12 bg-muted/20 border border-border/20">
				<div
					className="absolute bottom-0 left-0 right-0 bg-orange-400/60 transition-all duration-75"
					style={{ height: `${pct}%` }}
				/>
			</div>
			<span className="text-[8px] text-muted-foreground/60 tracking-wide leading-tight text-center truncate w-full">
				{label}
			</span>
		</div>
	);
}

// ─── pad grid ─────────────────────────────────────────────────────────────────

function PadGrid({
	cues,
	ctrlState,
	editMode,
	onToggle,
	onEditCue,
	onCreateCue,
	onReorder,
}: {
	cues: Cue[];
	ctrlState: ControllerState | null;
	editMode: boolean;
	onToggle: (cueId: string) => void;
	onEditCue: (cue: Cue) => void;
	onCreateCue: () => void;
	onReorder: (reordered: Cue[]) => void;
}) {
	const [dragIndex, setDragIndex] = useState<number | null>(null);
	const [dragOverIndex, setDragOverIndex] = useState<number | null>(null);

	if (cues.length === 0 && !editMode) {
		return (
			<div className="text-center text-[10px] tracking-widest text-muted-foreground/40 uppercase py-4">
				No cues — tap EDIT to add one
			</div>
		);
	}

	const handleDragStart = (i: number) => setDragIndex(i);
	const handleDragOver = (e: React.DragEvent, i: number) => {
		e.preventDefault();
		setDragOverIndex(i);
	};
	const handleDrop = (e: React.DragEvent, targetIndex: number) => {
		e.preventDefault();
		if (dragIndex === null || dragIndex === targetIndex) {
			setDragIndex(null);
			setDragOverIndex(null);
			return;
		}
		const reordered = [...cues];
		const [moved] = reordered.splice(dragIndex, 1);
		reordered.splice(targetIndex, 0, moved);
		onReorder(reordered);
		setDragIndex(null);
		setDragOverIndex(null);
	};

	return (
		<div className="grid grid-cols-4 gap-1">
			{cues.map((cue, i) => {
				const isActive = ctrlState?.activeCueIds.includes(cue.id) ?? false;
				const isFlash = ctrlState?.flashCueIds.includes(cue.id) ?? false;
				const lit = isActive || isFlash;
				const colorIndex = i % DECK_COLORS.length;
				const color = DECK_COLORS[colorIndex];
				const isDragOver = dragOverIndex === i;

				if (editMode) {
					return (
						<button
							type="button"
							key={cue.id}
							draggable
							onDragStart={() => handleDragStart(i)}
							onDragOver={(e) => handleDragOver(e, i)}
							onDrop={(e) => handleDrop(e, i)}
							onDragEnd={() => {
								setDragIndex(null);
								setDragOverIndex(null);
							}}
							onClick={() => onEditCue(cue)}
							className={cn(
								"aspect-square flex flex-col items-center justify-center gap-1 border text-center p-1 cursor-pointer transition-colors",
								isDragOver
									? "border-orange-400/60 bg-orange-400/10"
									: "border-border/30 bg-muted/5 hover:bg-muted/15 hover:border-border/50",
							)}
						>
							<Pencil className="w-3 h-3 text-muted-foreground/40" />
							<span className="text-[9px] leading-tight tracking-wide uppercase line-clamp-2 text-muted-foreground/70">
								{cue.name}
							</span>
						</button>
					);
				}

				return (
					<button
						key={cue.id}
						type="button"
						onClick={() => onToggle(cue.id)}
						className={cn(
							"aspect-square flex flex-col items-center justify-center gap-1 border text-center transition-colors p-1",
							lit
								? "border-current"
								: "border-border/20 bg-muted/5 hover:bg-muted/10",
						)}
						style={
							lit
								? { borderColor: color, backgroundColor: `${color}18`, color }
								: undefined
						}
					>
						<div
							className="w-1 h-1 rounded-full"
							style={{ backgroundColor: lit ? color : "rgba(255,255,255,0.1)" }}
						/>
						<span
							className="text-[9px] leading-tight tracking-wide uppercase line-clamp-2"
							style={{ color: lit ? color : undefined }}
						>
							{cue.name}
						</span>
					</button>
				);
			})}

			{/* new cue pad — visible in edit mode */}
			{editMode && (
				<button
					type="button"
					onClick={onCreateCue}
					className="aspect-square flex flex-col items-center justify-center gap-1 border border-dashed border-border/30 text-muted-foreground/40 hover:border-border/60 hover:text-muted-foreground/60 transition-colors"
				>
					<Plus className="w-3 h-3" />
					<span className="text-[9px] tracking-wide uppercase">New</span>
				</button>
			)}
		</div>
	);
}

// ─── delete confirm ───────────────────────────────────────────────────────────

function DeleteConfirm({
	id,
	active,
	onRequest,
	onConfirm,
}: {
	id: string;
	active: string | null;
	onRequest: (id: string | null) => void;
	onConfirm: (id: string) => void;
}) {
	if (active === id) {
		return (
			<div className="flex items-center gap-1 shrink-0">
				<button
					type="button"
					onClick={() => onConfirm(id)}
					className="text-xs text-destructive hover:text-destructive/80 transition-colors"
				>
					delete
				</button>
				<button
					type="button"
					onClick={() => onRequest(null)}
					className="text-xs text-muted-foreground hover:text-foreground transition-colors"
				>
					cancel
				</button>
			</div>
		);
	}
	return (
		<button
			type="button"
			onClick={() => onRequest(id)}
			className="shrink-0 text-muted-foreground/40 hover:text-muted-foreground/70 transition-colors"
		>
			<Trash2 className="w-3.5 h-3.5" />
		</button>
	);
}

// ─── bottom bar ───────────────────────────────────────────────────────────────

function BottomBar({
	connectionStatus,
	source,
}: {
	connectionStatus: string;
	source: string | null;
}) {
	return (
		<div className="flex items-center justify-between px-4 h-6 shrink-0 border-t border-border/20">
			<div className="flex items-center gap-4">
				<StatusItem label="SOURCE">
					{source ? source.toUpperCase() : "NONE"}
				</StatusItem>
				<StatusItem label="LINK">
					{connectionStatus === "connected"
						? "CONNECTED"
						: connectionStatus === "connecting"
							? "SEARCHING"
							: "OFFLINE"}
				</StatusItem>
			</div>
		</div>
	);
}

function StatusItem({
	label,
	children,
}: {
	label: string;
	children: React.ReactNode;
}) {
	return (
		<span className="text-[10px] text-muted-foreground/60 tracking-wider">
			{label} <span className="text-muted-foreground">{children}</span>
		</span>
	);
}
