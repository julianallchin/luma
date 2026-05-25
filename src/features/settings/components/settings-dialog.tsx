import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { relaunch } from "@tauri-apps/plugin-process";
import { check } from "@tauri-apps/plugin-updater";
import { useEffect, useState } from "react";
import { Button } from "@/shared/components/ui/button";
import { Checkbox } from "@/shared/components/ui/checkbox";
import {
	Dialog,
	DialogContent,
	DialogTitle,
} from "@/shared/components/ui/dialog";
import { Input } from "@/shared/components/ui/input";
import { Label } from "@/shared/components/ui/label";
import { Slider } from "@/shared/components/ui/slider";
import { ToggleGroup } from "@/shared/components/ui/toggle-group";
import { useSettingsDialogStore } from "../stores/use-settings-dialog-store";

type UpdateState =
	| { status: "idle" }
	| { status: "checking" }
	| { status: "downloading"; version: string }
	| { status: "ready"; version: string }
	| { status: "up-to-date" }
	| { status: "error"; message: string };

type SettingsTab = "general" | "ai" | "artnet" | "about";

const OPENROUTER_KEY_STORAGE = "luma:openrouter-api-key";

type AppSettings = {
	audio_output_enabled: boolean;
	artnet_enabled: boolean;
	artnet_interface: string;
	artnet_broadcast: boolean;
	artnet_unicast_ip: string;
	artnet_net: number;
	artnet_subnet: number;
	max_dimmer: number;
};

type ArtNetNode = {
	ip: string;
	name: string;
	long_name: string;
	port_address: number;
	last_seen: number;
};

const TABS: { value: SettingsTab; label: string }[] = [
	{ value: "general", label: "General" },
	{ value: "ai", label: "AI" },
	{ value: "artnet", label: "Art-Net / DMX" },
	{ value: "about", label: "About" },
];

export function SettingsDialog() {
	const open = useSettingsDialogStore((s) => s.open);
	const setOpen = useSettingsDialogStore((s) => s.setOpen);

	return (
		<Dialog open={open} onOpenChange={setOpen}>
			<DialogContent
				className="sm:max-w-3xl h-[600px] p-0 gap-0 bg-card border-control-border"
				showCloseButton={false}
			>
				<DialogTitle className="sr-only">Settings</DialogTitle>
				{open && <SettingsContent />}
			</DialogContent>
		</Dialog>
	);
}

function SettingsContent() {
	const [activeTab, setActiveTab] = useState<SettingsTab>("general");
	const [settings, setSettings] = useState<AppSettings | null>(null);
	const [nodes, setNodes] = useState<ArtNetNode[]>([]);
	const [scanning, setScanning] = useState(false);
	const [maxDimmerDebounceHandle, setMaxDimmerDebounceHandle] =
		useState<ReturnType<typeof setTimeout> | null>(null);
	const [appVersion, setAppVersion] = useState("");
	const [openRouterKey, setOpenRouterKey] = useState(
		() => localStorage.getItem(OPENROUTER_KEY_STORAGE) ?? "",
	);
	const [updateState, setUpdateState] = useState<UpdateState>({
		status: "idle",
	});

	useEffect(() => {
		loadSettings();
		getVersion().then(setAppVersion);
	}, []);

	useEffect(() => {
		let interval: ReturnType<typeof setInterval>;
		if (scanning) {
			invoke("start_discovery").catch(console.error);
			interval = setInterval(async () => {
				const found = await invoke<ArtNetNode[]>("get_discovered_nodes");
				setNodes(found);
			}, 1000);
		}

		return () => {
			if (interval) clearInterval(interval);
			if (scanning) {
				invoke("stop_discovery").catch(console.error);
			}
		};
	}, [scanning]);

	const loadSettings = async () => {
		try {
			const s = await invoke<AppSettings>("get_settings");
			setSettings(s);
		} catch (e) {
			console.error("Failed to load settings", e);
		}
	};

	const updateSetting = async (key: string, value: string) => {
		try {
			await invoke("set_setting", { key, value });
			await loadSettings();
		} catch (e) {
			console.error("Failed to update setting", e);
		}
	};

	const updateMaxDimmer = (value: number) => {
		const clamped = Math.min(100, Math.max(0, Math.round(value)));
		setSettings((prev) => (prev ? { ...prev, max_dimmer: clamped } : prev));

		if (maxDimmerDebounceHandle) clearTimeout(maxDimmerDebounceHandle);
		setMaxDimmerDebounceHandle(
			setTimeout(() => {
				updateSetting("max_dimmer", String(clamped));
				setMaxDimmerDebounceHandle(null);
			}, 150),
		);
	};

	const checkForUpdates = async () => {
		setUpdateState({ status: "checking" });
		try {
			const update = await check();
			if (!update) {
				setUpdateState({ status: "up-to-date" });
				return;
			}
			setUpdateState({ status: "downloading", version: update.version });
			await update.downloadAndInstall();
			setUpdateState({ status: "ready", version: update.version });
		} catch (e) {
			setUpdateState({ status: "error", message: String(e) });
		}
	};

	const handleOpenRouterKeyChange = (value: string) => {
		setOpenRouterKey(value);
		const trimmed = value.trim();
		if (trimmed) {
			localStorage.setItem(OPENROUTER_KEY_STORAGE, trimmed);
		} else {
			localStorage.removeItem(OPENROUTER_KEY_STORAGE);
		}
		window.dispatchEvent(new Event("luma:openrouter-key-changed"));
	};

	if (!settings) {
		return (
			<div className="flex items-center justify-center h-full text-xs uppercase tracking-wider text-foreground/60">
				Loading…
			</div>
		);
	}

	return (
		<div className="flex flex-col h-full min-h-0">
			<header className="flex items-center justify-between px-3 h-9 border-b border-trim bg-titlebar">
				<span className="text-[9px] uppercase tracking-wider font-bold text-foreground/80">
					Settings
				</span>
				<ToggleGroup
					value={activeTab}
					options={TABS}
					onChange={(v) => setActiveTab(v as SettingsTab)}
				/>
			</header>

			<div className="p-4 flex-1 min-h-0 overflow-y-auto">
				{activeTab === "general" && (
					<div className="space-y-3">
						<div className="flex items-center space-x-2">
							<Checkbox
								id="audio-output-enabled"
								checked={settings.audio_output_enabled}
								onCheckedChange={(c) =>
									updateSetting("audio_output_enabled", String(!!c))
								}
							/>
							<Label htmlFor="audio-output-enabled">Enable Audio Output</Label>
						</div>
						<p className="text-xs text-foreground/60">
							When disabled, playback stays in sync but stays silent.
						</p>
					</div>
				)}

				{activeTab === "ai" && (
					<div className="space-y-3">
						<div className="space-y-2">
							<Label htmlFor="openrouter-api-key">OpenRouter API Key</Label>
							<Input
								id="openrouter-api-key"
								type="password"
								value={openRouterKey}
								onChange={(e) => handleOpenRouterKeyChange(e.target.value)}
								placeholder="sk-or-..."
								autoComplete="off"
								spellCheck={false}
							/>
							<p className="text-xs text-foreground/60">
								Used by the track editor chat sidebar. Get a key from{" "}
								<a
									href="https://openrouter.ai/keys"
									target="_blank"
									rel="noreferrer"
									className="underline hover:text-foreground"
								>
									openrouter.ai/keys
								</a>
								.
							</p>
						</div>
					</div>
				)}

				{activeTab === "about" && (
					<div className="space-y-3">
						<p className="text-xs uppercase tracking-wider text-foreground/60">
							Luma v{appVersion}
						</p>

						<div className="flex items-center gap-3">
							{updateState.status === "ready" ? (
								<Button onClick={() => relaunch()}>
									Restart to update to v{updateState.version}
								</Button>
							) : (
								<Button
									onClick={checkForUpdates}
									disabled={
										updateState.status === "checking" ||
										updateState.status === "downloading"
									}
								>
									{updateState.status === "checking"
										? "Checking..."
										: updateState.status === "downloading"
											? `Downloading v${updateState.version}...`
											: "Check for Updates"}
								</Button>
							)}
							{updateState.status === "up-to-date" && (
								<p className="text-xs text-foreground/60">
									You're on the latest version.
								</p>
							)}
							{updateState.status === "error" && (
								<p className="text-xs text-destructive">
									{updateState.message}
								</p>
							)}
						</div>
					</div>
				)}

				{activeTab === "artnet" && (
					<div className="space-y-5">
						<div className="grid gap-2">
							<Label htmlFor="max-dimmer">Max Brightness</Label>
							<Slider
								id="max-dimmer"
								min={0}
								max={100}
								step={1}
								value={settings.max_dimmer}
								onChange={(e) => updateMaxDimmer(Number(e.target.value))}
							/>
							<p className="text-xs text-foreground/60">
								Limits overall brightness of DMX output (100 = no limit).
							</p>
						</div>

						<div className="flex items-center space-x-2">
							<Checkbox
								id="artnet-enabled"
								checked={settings.artnet_enabled}
								onCheckedChange={(c) =>
									updateSetting("artnet_enabled", String(!!c))
								}
							/>
							<Label htmlFor="artnet-enabled">Enable Art-Net Output</Label>
						</div>

						<div className="grid grid-cols-2 gap-3">
							<div className="space-y-2">
								<Label>Interface IP (Bind Address)</Label>
								<Input
									value={settings.artnet_interface}
									onChange={(e) =>
										updateSetting("artnet_interface", e.target.value)
									}
									placeholder="0.0.0.0"
								/>
								<p className="text-xs text-foreground/60">
									0.0.0.0 binds to all interfaces.
								</p>
							</div>
							<div className="space-y-2">
								<Label>Unicast Destination IP</Label>
								<Input
									value={settings.artnet_unicast_ip}
									onChange={(e) =>
										updateSetting("artnet_unicast_ip", e.target.value)
									}
									placeholder="Leave empty for broadcast only"
								/>
							</div>
						</div>

						<div className="flex items-center space-x-2">
							<Checkbox
								id="artnet-broadcast"
								checked={settings.artnet_broadcast}
								onCheckedChange={(c) =>
									updateSetting("artnet_broadcast", String(!!c))
								}
							/>
							<Label htmlFor="artnet-broadcast">
								Always Broadcast (255.255.255.255)
							</Label>
						</div>

						<div className="grid grid-cols-2 gap-3">
							<div className="space-y-2">
								<Label>Net (0-127)</Label>
								<Input
									type="number"
									value={settings.artnet_net}
									onChange={(e) => updateSetting("artnet_net", e.target.value)}
								/>
							</div>
							<div className="space-y-2">
								<Label>Subnet (0-15)</Label>
								<Input
									type="number"
									value={settings.artnet_subnet}
									onChange={(e) =>
										updateSetting("artnet_subnet", e.target.value)
									}
								/>
							</div>
						</div>

						<div className="space-y-2">
							<div className="flex items-center justify-between">
								<span className="text-[9px] uppercase tracking-wider font-bold text-foreground/80">
									Discovered Nodes
								</span>
								<Button onClick={() => setScanning(!scanning)}>
									{scanning ? "Stop Scanning" : "Scan for Nodes"}
								</Button>
							</div>

							<div className="border border-control-border divide-y divide-trim bg-control">
								{nodes.length === 0 ? (
									<div className="p-3 text-center text-xs text-foreground/60">
										{scanning
											? "Scanning..."
											: "No nodes found. Click Scan to search."}
									</div>
								) : (
									nodes.map((node) => (
										<div
											key={node.ip}
											className="p-3 flex items-center justify-between hover:bg-hover transition-colors"
										>
											<div>
												<div className="text-xs font-medium">
													{node.name || "Unknown Node"}
												</div>
												<div className="text-[10px] text-foreground/60">
													{node.ip} • {node.long_name}
												</div>
											</div>
											<Button
												onClick={() =>
													updateSetting("artnet_unicast_ip", node.ip)
												}
											>
												Use as Unicast
											</Button>
										</div>
									))
								)}
							</div>
						</div>
					</div>
				)}
			</div>
		</div>
	);
}
