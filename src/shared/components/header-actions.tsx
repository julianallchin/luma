import { Window } from "@tauri-apps/api/window";
import { useAuthStore } from "@/features/auth/stores/use-auth-store";
import { Button } from "./ui/button";
import { Dropdown } from "./ui/dropdown";
import { WindowControls } from "./window-controls";

/// Right-hand cluster shown on every titlebar: settings button, account
/// dropdown (with sign out), and the custom window controls.
export function HeaderActions() {
	const logout = useAuthStore((s) => s.logout);
	const displayName = useAuthStore((s) => s.displayName);

	const openSettings = async () => {
		const settingsWindow = new Window("settings");
		await settingsWindow.show();
		await settingsWindow.setFocus();
	};

	return (
		<div className="no-drag flex items-center gap-2">
			<Button onClick={() => void openSettings()}>settings</Button>
			<Dropdown
				label={
					<span className="truncate max-w-[140px]">
						{displayName ?? "account"}
					</span>
				}
				items={[{ label: "Sign out", onClick: () => void logout() }]}
			/>
			<WindowControls />
		</div>
	);
}
