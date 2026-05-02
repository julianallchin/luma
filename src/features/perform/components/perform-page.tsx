import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Loader2, Pencil, Plus, Settings2, Trash2, X, Zap } from "lucide-react";
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
import type { DeckMatchState, MixerState } from "../stores/use-perform-store";
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
	const [mixerStatus, setMixerStatus] = useState<{
		connected: boolean;
		portName: string | null;
		availablePorts: string[];
	} | null>(null);
	const [showMixerDialog, setShowMixerDialog] = useState(false);
	const setMixerState = usePerformStore((s) => s.setMixerState);
	const groups = useGroupStore((s) => s.groups);
	const fetchGroups = useGroupStore((s) => s.fetchGroups);

	// controller panel state
	const [ctrlConfigOpen, setCtrlConfigOpen] = useState(false);
	const [ctrlTab, setCtrlTab] = useState<"cues" | "actions" | "modifiers">(
		"cues",
	);
	const [editMode, setEditMode] = useState(false);
	const [editingCue, setEditingCue] = useState<Cue | null>(null);
	const [creatingCueAt, setCreatingCueAt] = useState<{
		x: number;
		y: number;
	} | null>(null);

	// Initialize fixtures
	useEffect(() => {
		if (currentVenueId !== null) {
			useFixtureStore.getState().initialize(currentVenueId);
		} else {
			useFixtureStore.getState().initialize();
		}
	}, [currentVenueId]);

	// Init controller + mixer + compile sim deck
	useEffect(() => {
		if (currentVenueId === null) return;
		invoke("controller_init_for_venue", {
			controllerPort: currentVenue?.controllerPort ?? null,
		}).catch(() => {});
		invoke("mixer_init_for_venue", { venueId: currentVenueId }).catch(() => {});
		invoke("midi_reload_mapping", { venueId: currentVenueId }).catch(() => {});
		fetchGroups(currentVenueId);
	}, [currentVenueId, currentVenue?.controllerPort, fetchGroups]);

	// Poll mixer status every 2s (also triggers auto-reconnect on the Rust side)
	useEffect(() => {
		const pollMixer = async () => {
			try {
				setMixerStatus(
					await invoke<{
						connected: boolean;
						portName: string | null;
						availablePorts: string[];
					}>("mixer_get_status"),
				);
			} catch {}
		};
		pollMixer();
		const id = setInterval(pollMixer, 2000);
		return () => clearInterval(id);
	}, []);

	// Reset mixer state when device is unplugged (connected flips to false)
	const prevMixerConnected = useRef<boolean | null>(null);
	useEffect(() => {
		const connected = mixerStatus?.connected ?? false;
		if (prevMixerConnected.current === true && !connected) {
			setMixerState(null);
			setShowMixerDialog(false);
		}
		prevMixerConnected.current = connected;
	}, [mixerStatus?.connected, setMixerState]);

	// Real-time mixer fader state
	useEffect(() => {
		let unlisten: (() => void) | null = null;
		listen<MixerState>("mixer_state", (e) => {
			setMixerState(e.payload);
		}).then((fn) => {
			unlisten = fn;
		});
		return () => {
			unlisten?.();
		};
	}, [setMixerState]);

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

	// Reconnect on mount if we have a previous source (e.g. navigated away and back)
	useEffect(() => {
		usePerformStore.getState().reconnectIfNeeded();
	}, []);

	// Cleanup on unmount — stop lights but keep the CDJ/StageLinQ connection alive
	useEffect(() => {
		return () => {
			invoke("render_clear_perform").catch(() => {});
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

	const moveCue = async (cueId: string, x: number, y: number) => {
		setCues((prev) =>
			prev.map((c) =>
				c.id === cueId ? { ...c, displayX: x, displayY: y } : c,
			),
		);
		await invoke("midi_update_cue", {
			input: { id: cueId, displayX: x, displayY: y },
		}).catch(() => {});
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
						mixerStatus={mixerStatus}
						onConfigureMixer={() => setShowMixerDialog(true)}
						onDisconnectMixer={async () => {
							if (!currentVenueId) return;
							try {
								await invoke("mixer_disconnect", { venueId: currentVenueId });
								setMixerState(null);
								setMixerStatus((s) =>
									s ? { ...s, connected: false, portName: null } : s,
								);
							} catch {}
						}}
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
									onCreateCue={(x, y) => setCreatingCueAt({ x, y })}
									onMove={moveCue}
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

			{/* ── mixer setup dialog ── */}
			{currentVenueId && (
				<MixerSetupDialog
					open={showMixerDialog}
					onOpenChange={setShowMixerDialog}
					venueId={currentVenueId}
					onConnected={(state) => {
						setMixerState(state);
						setMixerStatus((s) => (s ? { ...s, connected: true } : s));
					}}
				/>
			)}

			{/* ── cue editor dialog ── */}
			{(editingCue || creatingCueAt !== null) && currentVenueId && (
				<CueEditorDialog
					cue={editingCue}
					createAt={creatingCueAt ?? undefined}
					isOpen={true}
					onClose={() => {
						setEditingCue(null);
						setCreatingCueAt(null);
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
					onSaved={() => {
						setEditingCue(null);
						setCreatingCueAt(null);
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

// ─── mixer setup dialog ───────────────────────────────────────────────────────

interface LearnedCc {
	channel: number;
	cc: number;
}

type LearnTarget = "fader_1" | "fader_2" | "fader_3" | "fader_4" | "crossfader";

function MixerSetupDialog({
	open,
	onOpenChange,
	venueId,
	onConnected,
}: {
	open: boolean;
	onOpenChange: (v: boolean) => void;
	venueId: string;
	onConnected: (state: MixerState) => void;
}) {
	const [availablePorts, setAvailablePorts] = useState<string[]>([]);
	const [selectedPort, setSelectedPort] = useState<string>("");
	const [mapping, setMapping] = useState<Record<LearnTarget, LearnedCc | null>>(
		{
			fader_1: null,
			fader_2: null,
			fader_3: null,
			fader_4: null,
			crossfader: null,
		},
	);
	const [learning, setLearning] = useState<LearnTarget | null>(null);
	const [portOpen, setPortOpen] = useState(false);
	const [saving, setSaving] = useState(false);
	const [error, setError] = useState<string | null>(null);

	// Poll ports while dialog is open so plug-in is detected immediately
	useEffect(() => {
		if (!open) return;
		setError(null);
		const poll = () =>
			invoke<string[]>("mixer_list_ports")
				.then((ports) => {
					setAvailablePorts(ports);
					setSelectedPort((prev) => {
						if (prev) return prev;
						return ports.length > 0 ? ports[0] : prev;
					});
				})
				.catch(() => {});
		poll();
		const id = setInterval(poll, 1500);
		return () => clearInterval(id);
	}, [open]);

	// Listen for learn captures
	useEffect(() => {
		if (!open || !learning) return;
		let unlisten: (() => void) | null = null;
		listen<LearnedCc>("mixer_learned", (e) => {
			setMapping((prev) => {
				const updated = { ...prev };
				// Clear any other target that already uses the same channel+CC
				for (const key of Object.keys(updated) as LearnTarget[]) {
					const existing = updated[key];
					if (
						key !== learning &&
						existing?.channel === e.payload.channel &&
						existing?.cc === e.payload.cc
					) {
						updated[key] = null;
					}
				}
				updated[learning] = e.payload;
				return updated;
			});
			setLearning(null);
		}).then((fn) => {
			unlisten = fn;
		});
		return () => {
			unlisten?.();
			invoke("mixer_cancel_learn").catch(() => {});
		};
	}, [open, learning]);

	const openPort = async (port: string) => {
		setError(null);
		try {
			await invoke("mixer_open_port", { portName: port });
			setPortOpen(true);
		} catch (e) {
			setError(String(e));
		}
	};

	const startLearn = async (target: LearnTarget) => {
		if (!portOpen) {
			await openPort(selectedPort);
		}
		setLearning(target);
		invoke("mixer_start_learn").catch(() => {});
	};

	const save = async () => {
		setSaving(true);
		setError(null);
		try {
			const channelFaders: Record<number, { channel: number; cc: number }> = {};
			if (mapping.fader_1) channelFaders[1] = mapping.fader_1;
			if (mapping.fader_2) channelFaders[2] = mapping.fader_2;
			if (mapping.fader_3) channelFaders[3] = mapping.fader_3;
			if (mapping.fader_4) channelFaders[4] = mapping.fader_4;

			const mixerMapping = {
				channelFaders,
				crossfader: mapping.crossfader,
			};

			await invoke("mixer_connect", {
				venueId,
				portName: selectedPort,
				mapping: mixerMapping,
			});

			// Build initial state (all faders at 1.0 until MIDI moves them)
			const initialState: MixerState = {
				channelFaders: Object.fromEntries(
					Object.keys(channelFaders).map((k) => [Number(k), 1.0]),
				),
				crossfader: 0.5,
			};
			onConnected(initialState);
			onOpenChange(false);
		} catch (e) {
			setError(String(e));
		} finally {
			setSaving(false);
		}
	};

	const labelCc = (spec: LearnedCc | null) =>
		spec ? `ch${spec.channel + 1} cc${spec.cc}` : null;

	const faderControls: { target: LearnTarget; label: string }[] = [
		{ target: "fader_1", label: "Fader 1" },
		{ target: "fader_2", label: "Fader 2" },
		{ target: "fader_3", label: "Fader 3" },
		{ target: "fader_4", label: "Fader 4" },
		{ target: "crossfader", label: "Crossfader" },
	];

	const hasSomeMapped = Object.values(mapping).some((v) => v !== null);

	return (
		<Dialog open={open} onOpenChange={onOpenChange}>
			<DialogContent className="max-w-sm">
				<DialogHeader>
					<DialogTitle className="text-sm font-medium">
						MIDI Mixer Setup
					</DialogTitle>
				</DialogHeader>

				<div className="space-y-4">
					{/* Port selector */}
					<div className="space-y-1.5">
						<span className="text-xs text-muted-foreground uppercase tracking-wider">
							MIDI Port
						</span>
						{availablePorts.length > 0 ? (
							<Select
								value={selectedPort}
								onValueChange={(v) => {
									setSelectedPort(v);
									setPortOpen(false);
									setLearning(null);
								}}
							>
								<SelectTrigger className="h-8 text-sm">
									<SelectValue placeholder="Select port…" />
								</SelectTrigger>
								<SelectContent>
									{availablePorts.map((p) => (
										<SelectItem key={p} value={p} className="text-sm">
											{p}
										</SelectItem>
									))}
								</SelectContent>
							</Select>
						) : (
							<div className="text-xs text-muted-foreground/60 py-1">
								No MIDI ports found
							</div>
						)}
					</div>

					{/* Learn controls */}
					<div className="space-y-1.5">
						<span className="text-xs text-muted-foreground uppercase tracking-wider">
							Fader mapping — move each fader after clicking Learn
						</span>
						<div className="border border-border/40 divide-y divide-border/20">
							{faderControls.map(({ target, label }) => {
								const captured = labelCc(mapping[target]);
								const isLearning = learning === target;
								return (
									<div
										key={target}
										className="flex items-center justify-between px-3 py-2"
									>
										<span className="text-xs">{label}</span>
										<div className="flex items-center gap-2">
											{isLearning ? (
												<span className="text-xs text-muted-foreground animate-pulse">
													move fader…
												</span>
											) : captured ? (
												<span className="text-[10px] font-mono text-muted-foreground">
													{captured}
												</span>
											) : (
												<span className="text-[10px] text-muted-foreground/40">
													—
												</span>
											)}
											{captured && !isLearning && (
												<Button
													variant="ghost"
													size="sm"
													className="h-6 w-6 p-0 text-muted-foreground/50 hover:text-destructive"
													onClick={() =>
														setMapping((prev) => ({ ...prev, [target]: null }))
													}
												>
													<X className="h-3 w-3" />
												</Button>
											)}
											<Button
												variant="outline"
												size="sm"
												className="h-6 text-[10px] px-2"
												disabled={!selectedPort || isLearning}
												onClick={() => startLearn(target)}
											>
												{isLearning ? "…" : "Learn"}
											</Button>
										</div>
									</div>
								);
							})}
						</div>
						<p className="text-[10px] text-muted-foreground/50">
							Only map the controls you want Luma to read — unmapped controls
							are ignored.
						</p>
					</div>

					{error && <p className="text-xs text-destructive">{error}</p>}

					<div className="flex items-center justify-end gap-2 pt-1">
						<Button
							variant="ghost"
							size="sm"
							onClick={() => {
								invoke("mixer_cancel_learn").catch(() => {});
								onOpenChange(false);
							}}
						>
							Cancel
						</Button>
						<Button
							size="sm"
							onClick={save}
							disabled={saving || !selectedPort || !hasSomeMapped}
						>
							{saving ? "Saving…" : "Save"}
						</Button>
					</div>
				</div>
			</DialogContent>
		</Dialog>
	);
}

// ─── cue editor dialog ────────────────────────────────────────────────────────

function CueEditorDialog({
	cue,
	createAt,
	isOpen,
	onClose,
	venueId,
	patterns,
	existingBinding,
	onSaved,
	onDeleted,
}: {
	cue: Cue | null;
	createAt?: { x: number; y: number };
	isOpen: boolean;
	onClose: () => void;
	venueId: string;
	patterns: PatternSummary[];
	existingBinding: MidiBinding | null;
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
						displayX: createAt?.x ?? 0,
						displayY: createAt?.y ?? 0,
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

// ─── Pioneer connect dialog ───────────────────────────────────────────────────

interface DiscoveredCDJ {
	player: number;
	name: string;
	ip: string;
}

function PioneerConnectDialog({
	open,
	onOpenChange,
	onConnect,
}: {
	open: boolean;
	onOpenChange: (v: boolean) => void;
	onConnect: (deviceNum: number) => void;
}) {
	const [deviceNum, setDeviceNum] = useState(7);
	const [discovering, setDiscovering] = useState(false);
	const [discovered, setDiscovered] = useState<DiscoveredCDJ[]>([]);

	useEffect(() => {
		if (!open) return;
		setDiscovering(true);
		setDiscovered([]);
		invoke<DiscoveredCDJ[]>("prodjlink_discover")
			.then((devices) => {
				setDiscovered(devices);
				setDiscovering(false);
			})
			.catch(() => setDiscovering(false));
	}, [open]);

	const handleConnect = () => {
		onConnect(deviceNum);
		onOpenChange(false);
	};

	return (
		<Dialog open={open} onOpenChange={onOpenChange}>
			<DialogContent className="max-w-sm">
				<DialogHeader>
					<DialogTitle className="text-sm font-medium">
						Connect Pro DJ Link
					</DialogTitle>
				</DialogHeader>

				<div className="space-y-4">
					{/* Discovery results */}
					<div className="space-y-1.5">
						<div className="flex items-center gap-2">
							<span className="text-xs text-muted-foreground uppercase tracking-wider">
								CDJs on network
							</span>
							{discovering && (
								<Loader2 className="w-3 h-3 animate-spin text-muted-foreground" />
							)}
						</div>
						{discovering ? (
							<div className="text-xs text-muted-foreground/60 py-2">
								Searching…
							</div>
						) : discovered.length > 0 ? (
							<div className="border border-border/40 divide-y divide-border/20">
								{discovered.map((d) => (
									<div
										key={d.player}
										className="flex items-center justify-between px-3 py-2"
									>
										<span className="text-xs font-medium">
											{d.name || "CDJ"} (Player {d.player})
										</span>
										<span className="text-[10px] text-muted-foreground font-mono">
											{d.ip}
										</span>
									</div>
								))}
							</div>
						) : (
							<div className="text-xs text-muted-foreground/60 py-2">
								No CDJs found — check network connection
							</div>
						)}
					</div>

					{/* Virtual player number */}
					<div className="space-y-1.5">
						<span className="text-xs text-muted-foreground uppercase tracking-wider">
							Luma virtual player number
						</span>
						<Select
							value={String(deviceNum)}
							onValueChange={(v) => setDeviceNum(Number(v))}
						>
							<SelectTrigger className="h-8 text-sm">
								<SelectValue />
							</SelectTrigger>
							<SelectContent>
								{[5, 6, 7, 8, 9].map((n) => (
									<SelectItem key={n} value={String(n)} className="text-sm">
										Player {n}
									</SelectItem>
								))}
							</SelectContent>
						</Select>
						<p className="text-[10px] text-muted-foreground/60">
							Pick a number not used by any CDJ (CDJs are typically 1–4)
						</p>
					</div>

					<div className="flex items-center justify-end gap-2 pt-1">
						<Button
							variant="ghost"
							size="sm"
							onClick={() => onOpenChange(false)}
						>
							Cancel
						</Button>
						<Button size="sm" onClick={handleConnect}>
							Connect
						</Button>
					</div>
				</div>
			</DialogContent>
		</Dialog>
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
	mixerStatus,
	onConfigureMixer,
	onDisconnectMixer,
}: {
	connectionStatus: string;
	deviceName: string | null;
	error: string | null;
	decks: DeckState[];
	deckMatches: Map<number, DeckMatchState>;
	activeDeckId: number | null;
	crossfader: number;
	onConnect: (source: "stagelinq" | "prodjlink", deviceNum?: number) => void;
	onDisconnect: () => void;
	mixerStatus: {
		connected: boolean;
		portName: string | null;
		availablePorts: string[];
	} | null;
	onConfigureMixer: () => void;
	onDisconnectMixer: () => void;
}) {
	const mixerState = usePerformStore((s) => s.mixerState);
	const [showSourceMenu, setShowSourceMenu] = useState(false);
	const [showPioneerDialog, setShowPioneerDialog] = useState(false);
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
				<div className="flex items-center gap-3">
					{/* mixer connect/disconnect */}
					{mixerStatus?.connected ? (
						<div className="flex items-center gap-2">
							<span className="text-[10px] text-muted-foreground truncate max-w-24">
								{mixerStatus.portName}
							</span>
							<button
								type="button"
								onClick={onDisconnectMixer}
								className="text-[10px] text-muted-foreground/70 hover:text-muted-foreground transition-colors"
							>
								disconnect
							</button>
						</div>
					) : (
						<button
							type="button"
							onClick={onConfigureMixer}
							className="text-[10px] text-muted-foreground/70 hover:text-muted-foreground transition-colors"
						>
							+ connect mixer
						</button>
					)}

					<span className="text-muted-foreground/30 text-[10px]">|</span>

					{/* source connect/disconnect */}
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
									className="w-full px-3 py-1.5 text-left text-xs text-muted-foreground hover:text-foreground hover:bg-muted/30 transition-colors"
									onClick={() => {
										setShowPioneerDialog(true);
										setShowSourceMenu(false);
									}}
								>
									Pro DJ Link (Pioneer)
								</button>
							</div>
						)}
					</div>
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
					<span className="text-[10px] tracking-wider text-muted-foreground/50 uppercase w-12">
						X
					</span>
					<div className="h-3 bg-muted/20 flex-1 relative rounded-sm overflow-hidden">
						<div
							className="absolute inset-y-0 bg-foreground/20"
							style={{
								width: `${((mixerState?.crossfader ?? crossfader) * 100).toFixed(0)}%`,
							}}
						/>
						<div
							className="absolute top-0 bottom-0 w-0.5 bg-foreground/60"
							style={{
								left: `${((mixerState?.crossfader ?? crossfader) * 100).toFixed(0)}%`,
							}}
						/>
					</div>
				</div>
			)}

			<PioneerConnectDialog
				open={showPioneerDialog}
				onOpenChange={setShowPioneerDialog}
				onConnect={(deviceNum) => onConnect("prodjlink", deviceNum)}
			/>
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
	const mixerState = usePerformStore((s) => s.mixerState);
	const colorIndex = (deck.id - 1) % DECK_COLORS.length;
	const color = DECK_COLORS[colorIndex];
	const bpm = deck.beat_bpm > 0 ? deck.beat_bpm : deck.bpm;
	const beatInBar = deck.beat > 0 ? (Math.floor(deck.beat) % 4) + 1 : 0;
	const progress =
		deck.total_beats > 0 ? (deck.beat / deck.total_beats) * 100 : 0;

	// Fader value: prefer mixer MIDI if connected, fall back to deck's own fader
	const faderValue = mixerState
		? (mixerState.channelFaders[deck.id] ?? 1.0)
		: deck.fader;

	return (
		<div
			className={cn(
				"flex border-b border-border/20 transition-colors",
				isActive ? "bg-muted/10" : "",
			)}
		>
			{/* main content */}
			<div className="flex-1 min-w-0 px-3 py-2">
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
										Math.ceil(beatInBar) === b
											? color
											: "rgba(255,255,255,0.08)",
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

			{/* channel fader */}
			<div className="flex flex-col items-center justify-end w-5 py-1.5 px-1 border-l border-border/20">
				<div className="flex-1 w-full bg-muted/20 rounded-sm overflow-hidden flex flex-col justify-end">
					<div
						className="w-full rounded-sm"
						style={{
							height: `${(faderValue * 100).toFixed(0)}%`,
							backgroundColor: color,
							opacity: 0.6,
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
					className="absolute bottom-0 left-0 right-0 bg-orange-400/60"
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

const GRID_COLS = 4;

function PadGrid({
	cues,
	ctrlState,
	editMode,
	onToggle,
	onEditCue,
	onCreateCue,
	onMove,
}: {
	cues: Cue[];
	ctrlState: ControllerState | null;
	editMode: boolean;
	onToggle: (cueId: string) => void;
	onEditCue: (cue: Cue) => void;
	onCreateCue: (x: number, y: number) => void;
	onMove: (cueId: string, x: number, y: number) => void;
}) {
	const [draggingId, setDraggingId] = useState<string | null>(null);
	const [dragTarget, setDragTarget] = useState<{
		x: number;
		y: number;
	} | null>(null);

	// Build position lookup map
	const posMap = useMemo(() => {
		const m = new Map<string, Cue>();
		for (const c of cues) m.set(`${c.displayX},${c.displayY}`, c);
		return m;
	}, [cues]);

	// Color by sort order (y ASC, x ASC)
	const colorByCueId = useMemo(() => {
		const m = new Map<string, string>();
		for (let i = 0; i < cues.length; i++) {
			m.set(cues[i].id, DECK_COLORS[i % DECK_COLORS.length]);
		}
		return m;
	}, [cues]);

	const maxX = cues.length > 0 ? Math.max(...cues.map((c) => c.displayX)) : -1;
	const maxY = cues.length > 0 ? Math.max(...cues.map((c) => c.displayY)) : -1;

	// In edit mode show one extra row+col so there's always room to place new cues
	const cols = editMode
		? Math.max(maxX + 2, GRID_COLS)
		: Math.max(maxX + 1, GRID_COLS);
	const rows = editMode ? Math.max(maxY + 2, 2) : Math.max(maxY + 1, 1);

	const handleDragOver = (e: React.DragEvent, x: number, y: number) => {
		e.preventDefault();
		setDragTarget({ x, y });
	};
	const handleDrop = (e: React.DragEvent, x: number, y: number) => {
		e.preventDefault();
		if (!draggingId) return;
		const occupant = posMap.get(`${x},${y}`);
		// Only move to empty cells (or back to own cell)
		if (!occupant || occupant.id === draggingId) {
			onMove(draggingId, x, y);
		}
		setDraggingId(null);
		setDragTarget(null);
	};
	const handleDragEnd = () => {
		setDraggingId(null);
		setDragTarget(null);
	};

	if (cues.length === 0 && !editMode) {
		return (
			<div className="text-center text-[10px] tracking-widest text-muted-foreground/40 uppercase py-4">
				No cues — tap EDIT to add one
			</div>
		);
	}

	if (!editMode) {
		// Play mode: place cues at their explicit grid positions, no empty cells
		return (
			<div
				style={{
					display: "grid",
					gridTemplateColumns: `repeat(${GRID_COLS}, 1fr)`,
					gap: "4px",
				}}
			>
				{cues.map((cue) => {
					const isActive = ctrlState?.activeCueIds.includes(cue.id) ?? false;
					const isFlash = ctrlState?.flashCueIds.includes(cue.id) ?? false;
					const lit = isActive || isFlash;
					const color = colorByCueId.get(cue.id) ?? DECK_COLORS[0];
					return (
						<button
							key={cue.id}
							type="button"
							onClick={() => onToggle(cue.id)}
							style={{
								gridColumn: cue.displayX + 1,
								gridRow: cue.displayY + 1,
								...(lit
									? {
											borderColor: color,
											backgroundColor: `${color}18`,
											color,
										}
									: undefined),
							}}
							className={cn(
								"aspect-square flex flex-col items-center justify-center gap-1 border text-center transition-colors p-1",
								lit
									? "border-current"
									: "border-border/20 bg-muted/5 hover:bg-muted/10",
							)}
						>
							<div
								className="w-1 h-1 rounded-full"
								style={{
									backgroundColor: lit ? color : "rgba(255,255,255,0.1)",
								}}
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
			</div>
		);
	}

	// Edit mode: render full bounding-box grid including empty cells
	const cells: React.ReactNode[] = [];
	for (let y = 0; y < rows; y++) {
		for (let x = 0; x < cols; x++) {
			const cue = posMap.get(`${x},${y}`);
			const isDragTarget = dragTarget?.x === x && dragTarget?.y === y;

			if (cue) {
				cells.push(
					<button
						type="button"
						key={cue.id}
						draggable
						onDragStart={() => setDraggingId(cue.id)}
						onDragOver={(e) => handleDragOver(e, x, y)}
						onDrop={(e) => handleDrop(e, x, y)}
						onDragEnd={handleDragEnd}
						onClick={() => onEditCue(cue)}
						style={{ gridColumn: x + 1, gridRow: y + 1 }}
						className={cn(
							"aspect-square flex flex-col items-center justify-center gap-1 border text-center p-1 cursor-pointer transition-colors",
							isDragTarget
								? "border-orange-400/60 bg-orange-400/10"
								: draggingId === cue.id
									? "border-border/20 opacity-40"
									: "border-border/30 bg-muted/5 hover:bg-muted/15 hover:border-border/50",
						)}
					>
						<Pencil className="w-3 h-3 text-muted-foreground/40" />
						<span className="text-[9px] leading-tight tracking-wide uppercase line-clamp-2 text-muted-foreground/70">
							{cue.name}
						</span>
					</button>,
				);
			} else {
				cells.push(
					<button
						type="button"
						key={`empty-${x}-${y}`}
						onDragOver={(e) => handleDragOver(e, x, y)}
						onDrop={(e) => handleDrop(e, x, y)}
						onClick={() => onCreateCue(x, y)}
						style={{ gridColumn: x + 1, gridRow: y + 1 }}
						className={cn(
							"aspect-square flex flex-col items-center justify-center gap-1 border border-dashed text-center p-1 transition-colors",
							isDragTarget
								? "border-orange-400/60 bg-orange-400/10 text-orange-400/60"
								: "border-border/20 text-muted-foreground/30 hover:border-border/50 hover:text-muted-foreground/50",
						)}
					>
						<Plus className="w-3 h-3" />
					</button>,
				);
			}
		}
	}

	return (
		<div
			style={{
				display: "grid",
				gridTemplateColumns: `repeat(${cols}, 1fr)`,
				gap: "4px",
			}}
		>
			{cells}
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
