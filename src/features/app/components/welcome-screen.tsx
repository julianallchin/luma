import { invoke } from "@tauri-apps/api/core";
import { Button } from "@/shared/components/ui/button";
import { useAuthStore } from "@/features/auth/stores/use-auth-store";
import { toast } from "sonner";

export function WelcomeScreen() {
	const { logout } = useAuthStore();

	const handleNewVenue = () => {
		toast("lol u gotta build this");
	};

	const handleBrowseVenues = () => {
		toast("lol u gotta build this");
	};

	const handleSignOut = async () => {
		try {
			await logout();
		} catch {
			// Error handled by store
		}
	};

	const handleAuthDebug = async () => {
		try {
			await invoke("log_session_from_state_db");
		} catch (e) {
			console.error("Failed to log state session", e);
		}
	};

	return (
		<div className="relative h-full w-full bg-background text-foreground">
			<div className="absolute top-6 right-6 z-10">
				<Button onClick={handleSignOut} variant="ghost">
					sign out
				</Button>
			</div>
			<div className="absolute top-1/2 left-1/2 -translate-x-1/2 -translate-y-1/2 flex flex-col items-center gap-8">
				<h1 className="text-6xl font-extralight tracking-[0.2em] opacity-80 select-none">
					luma
				</h1>

				<div className="grid grid-rows-2 grid-cols-3 gap-4 w-2xl">
					<div className="bg-input border h-36" />
					<div className="bg-input border h-36" />
					<div className="bg-input border h-36" />
					<div className="bg-input border h-36" />
					<div className="bg-input border h-36" />
					<div className="bg-input border h-36" />
				</div>

				<div className="flex flex-col gap-4 w-64 z-10">
					<div className="flex gap-3 w-full justify-center">
						<Button
							onClick={handleNewVenue}
							variant="outline"
							className="w-full"
						>
							new venue
						</Button>
						<Button
							onClick={handleBrowseVenues}
							variant="outline"
							className="w-full"
						>
							browse venues
						</Button>
					</div>
					<Button onClick={handleAuthDebug} variant="ghost" className="w-full">
						debug auth
					</Button>
				</div>

				<div className="absolute top-full left-1/2 -translate-x-1/2 mt-12 w-80" />
			</div>
		</div>
	);
}
