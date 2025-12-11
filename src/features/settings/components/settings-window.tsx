import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { cn } from "@/shared/lib/utils";
import { Checkbox } from "@/shared/components/ui/checkbox";
import { Input } from "@/shared/components/ui/input";
import { Label } from "@/shared/components/ui/label";
import { Button } from "@/shared/components/ui/button";

type SettingsTab = "general" | "artnet";

type AppSettings = {
    artnet_enabled: boolean;
    artnet_interface: string;
    artnet_broadcast: boolean;
    artnet_unicast_ip: string;
    artnet_net: number;
    artnet_subnet: number;
};

type ArtNetNode = {
    ip: string;
    name: string;
    long_name: string;
    port_address: number;
    last_seen: number;
};

export function SettingsWindow() {
    const [activeTab, setActiveTab] = useState<SettingsTab>("general");
    const [settings, setSettings] = useState<AppSettings | null>(null);
    const [nodes, setNodes] = useState<ArtNetNode[]>([]);
    const [scanning, setScanning] = useState(false);

    useEffect(() => {
        loadSettings();
        
        const appWindow = getCurrentWindow();
        const unlisten = appWindow.onCloseRequested(async (event) => {
            event.preventDefault();
            await appWindow.hide();
        });
        return () => {
            unlisten.then((f) => f());
        };
    }, []);

    // Effect for scanning
    useEffect(() => {
        let interval: ReturnType<typeof setInterval>;
        if (scanning) {
            invoke("start_discovery").catch(console.error);
            interval = setInterval(async () => {
                const found = await invoke<ArtNetNode[]>("get_discovered_nodes");
                setNodes(found);
            }, 1000);
        }
        return () => clearInterval(interval);
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

    const tabs: { id: SettingsTab; label: string }[] = [
        { id: "general", label: "General" },
        { id: "artnet", label: "Art-Net / DMX" },
    ];

    if (!settings) return <div className="flex items-center justify-center h-screen">Loading settings...</div>;

    return (
        <div className="w-screen h-screen bg-muted flex text-foreground select-none">
            <div className="fixed top-0 left-0 w-full h-8 z-50 bg-transparent" data-tauri-drag-region />

            <div className="w-48 bg-card border-r border-border flex flex-col pt-10 pb-4">
                <div className="px-4 mb-2 text-xs font-semibold text-muted-foreground uppercase tracking-wider">
                    Settings
                </div>
                <nav className="flex-1 px-2 space-y-1">
                    {tabs.map((tab) => (
                        <button
                            key={tab.id}
                            onClick={() => setActiveTab(tab.id)}
                            className={cn(
                                "w-full text-left px-3 py-2 rounded-md text-sm transition-colors",
                                activeTab === tab.id
                                    ? "bg-accent text-accent-foreground font-medium"
                                    : "text-muted-foreground hover:bg-accent/50 hover:text-foreground"
                            )}
                        >
                            {tab.label}
                        </button>
                    ))}
                </nav>
            </div>

            <div className="flex-1 overflow-y-auto pt-10 p-8">
                <div className="max-w-2xl mx-auto space-y-8">
                    {activeTab === "general" && (
                        <div className="space-y-4">
                            <h2 className="text-2xl font-semibold tracking-tight">General</h2>
                            <p className="text-sm text-muted-foreground">General application settings.</p>
                             <div className="h-32 border-2 border-dashed border-border rounded-lg flex items-center justify-center text-muted-foreground text-sm">
                                No general settings yet.
                            </div>
                        </div>
                    )}

                    {activeTab === "artnet" && (
                        <div className="space-y-6">
                            <div>
                                <h2 className="text-2xl font-semibold tracking-tight">Art-Net / DMX</h2>
                                <p className="text-sm text-muted-foreground">Configure Art-Net output settings.</p>
                            </div>
                            
                            <div className="flex items-center space-x-2">
                                <Checkbox 
                                    id="artnet-enabled" 
                                    checked={settings.artnet_enabled}
                                    onCheckedChange={(c) => updateSetting("artnet_enabled", String(!!c))}
                                />
                                <Label htmlFor="artnet-enabled">Enable Art-Net Output</Label>
                            </div>

                            <div className="grid gap-4 border p-4 rounded-md bg-card">
                                <div className="grid grid-cols-2 gap-4">
                                    <div className="space-y-2">
                                        <Label>Interface IP (Bind Address)</Label>
                                        <Input 
                                            value={settings.artnet_interface}
                                            onChange={(e) => updateSetting("artnet_interface", e.target.value)}
                                            placeholder="0.0.0.0"
                                        />
                                        <p className="text-xs text-muted-foreground">0.0.0.0 binds to all interfaces.</p>
                                    </div>
                                    <div className="space-y-2">
                                        <Label>Unicast Destination IP</Label>
                                        <Input 
                                            value={settings.artnet_unicast_ip}
                                            onChange={(e) => updateSetting("artnet_unicast_ip", e.target.value)}
                                            placeholder="Leave empty for broadcast only"
                                        />
                                    </div>
                                </div>

                                <div className="flex items-center space-x-2">
                                    <Checkbox 
                                        id="artnet-broadcast" 
                                        checked={settings.artnet_broadcast}
                                        onCheckedChange={(c) => updateSetting("artnet_broadcast", String(!!c))}
                                    />
                                    <Label htmlFor="artnet-broadcast">Always Broadcast (255.255.255.255)</Label>
                                </div>
                                
                                <div className="grid grid-cols-2 gap-4">
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
                                            onChange={(e) => updateSetting("artnet_subnet", e.target.value)}
                                        />
                                    </div>
                                </div>
                            </div>

                            <div className="space-y-4">
                                <div className="flex items-center justify-between">
                                    <h3 className="text-lg font-medium">Discovered Nodes</h3>
                                    <Button 
                                        variant={scanning ? "secondary" : "default"}
                                        onClick={() => setScanning(!scanning)}
                                    >
                                        {scanning ? "Stop Scanning" : "Scan for Nodes"}
                                    </Button>
                                </div>
                                
                                <div className="border rounded-md divide-y bg-card">
                                    {nodes.length === 0 ? (
                                        <div className="p-4 text-center text-sm text-muted-foreground">
                                            {scanning ? "Scanning..." : "No nodes found. Click Scan to search."}
                                        </div>
                                    ) : (
                                        nodes.map((node) => (
                                            <div key={node.ip} className="p-3 flex items-center justify-between hover:bg-accent/50 transition-colors">
                                                <div>
                                                    <div className="font-medium">{node.name || "Unknown Node"}</div>
                                                    <div className="text-xs text-muted-foreground">{node.ip} • {node.long_name}</div>
                                                </div>
                                                <div className="flex gap-2">
                                                    <Button size="sm" variant="outline" onClick={() => updateSetting("artnet_unicast_ip", node.ip)}>
                                                        Use as Unicast
                                                    </Button>
                                                </div>
                                            </div>
                                        ))
                                    )}
                                </div>
                            </div>
                        </div>
                    )}
                </div>
            </div>
        </div>
    );
}