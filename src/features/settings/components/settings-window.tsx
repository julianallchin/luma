import { useState, useEffect } from "react";
import { cn } from "@/shared/lib/utils";
import { getCurrentWindow } from "@tauri-apps/api/window";

type SettingsTab = "general" | "artnet";

export function SettingsWindow() {
	const [activeTab, setActiveTab] = useState<SettingsTab>("general");

	useEffect(() => {
		const appWindow = getCurrentWindow();
		const unlisten = appWindow.onCloseRequested(async (event) => {
			event.preventDefault();
			await appWindow.hide();
		});
		return () => {
			unlisten.then((f) => f());
		};
	}, []);

	const tabs: { id: SettingsTab; label: string }[] = [
		{ id: "general", label: "General" },
		{ id: "artnet", label: "Art-Net / DMX" },
	];

	return (
		<div className="w-screen h-screen bg-muted flex text-foreground select-none">
			{/* Titlebar drag region for macOS style */}
			<div
				className="fixed top-0 left-0 w-full h-8 z-50 bg-transparent"
				data-tauri-drag-region
			/>

			{/* Sidebar */}
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
									: "text-muted-foreground hover:bg-accent/50 hover:text-foreground",
							)}
						>
							{tab.label}
						</button>
					))}
				</nav>
			</div>

			{/* Main Content */}
			<div className="flex-1 overflow-y-auto pt-10 p-8">
				<div className="max-w-2xl mx-auto space-y-8">
					{activeTab === "general" && (
						<div className="space-y-4">
							<h2 className="text-2xl font-semibold tracking-tight">General</h2>
							<p className="text-sm text-muted-foreground">
								General application settings will appear here.
							</p>
							{/* Placeholder for future settings */}
							<div className="h-32 border-2 border-dashed border-border rounded-lg flex items-center justify-center text-muted-foreground text-sm">
								No settings available yet
							</div>
						</div>
					)}

					{activeTab === "artnet" && (
						<div className="space-y-4">
							<h2 className="text-2xl font-semibold tracking-tight">
								Art-Net / DMX
							</h2>
							<p className="text-sm text-muted-foreground">
								Configure DMX output and Art-Net nodes.
							</p>
							{/* Placeholder for future settings */}
							<div className="h-32 border-2 border-dashed border-border rounded-lg flex items-center justify-center text-muted-foreground text-sm">
								No settings available yet
							</div>
						</div>
					)}
				</div>
			</div>
		</div>
	);
}
