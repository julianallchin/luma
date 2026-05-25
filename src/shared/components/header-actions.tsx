import { useAuthStore } from "@/features/auth/stores/use-auth-store";
import { useSettingsDialogStore } from "@/features/settings/stores/use-settings-dialog-store";
import { Button } from "./ui/button";
import { Dropdown } from "./ui/dropdown";
import { WindowControls } from "./window-controls";

/// Right-hand cluster shown on every titlebar: settings button, account
/// dropdown (with sign out), and the custom window controls.
export function HeaderActions() {
	const logout = useAuthStore((s) => s.logout);
	const displayName = useAuthStore((s) => s.displayName);
	const openSettings = useSettingsDialogStore((s) => s.setOpen);

	return (
		<div className="no-drag flex items-center gap-2">
			<Button onClick={() => openSettings(true)}>settings</Button>
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
