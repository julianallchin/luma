import { useEffect, useRef, useState } from "react";
import { Button } from "@/shared/components/ui/button";
import { Input } from "@/shared/components/ui/input";
import { toSnakeCase } from "@/shared/lib/utils";
import { checkUsernameAvailable } from "../lib/supabase";
import { useAuthStore } from "../stores/use-auth-store";

export function UsernameScreen() {
	const { email, isLoading, error, setUsername, clearError } = useAuthStore();
	const emailPrefix = email?.split("@")[0] ?? "";
	const [name, setName] = useState(emailPrefix);
	const [availability, setAvailability] = useState<
		"idle" | "checking" | "available" | "taken"
	>("idle");
	const debounceRef = useRef<ReturnType<typeof setTimeout>>(undefined);

	const normalizedName = toSnakeCase(name);

	// Debounced availability check
	useEffect(() => {
		clearTimeout(debounceRef.current);

		if (!normalizedName) {
			setAvailability("idle");
			return;
		}

		setAvailability("checking");
		debounceRef.current = setTimeout(async () => {
			const available = await checkUsernameAvailable(normalizedName);
			setAvailability(available ? "available" : "taken");
		}, 300);

		return () => clearTimeout(debounceRef.current);
	}, [normalizedName]);

	const handleSubmit = async (e: React.FormEvent) => {
		e.preventDefault();
		if (!normalizedName || availability !== "available") return;

		try {
			await setUsername(normalizedName);
		} catch {
			// Error is handled by the store
		}
	};

	const canSubmit =
		!isLoading && !!normalizedName && availability === "available";

	return (
		<div className="w-screen h-screen bg-background flex items-center justify-center">
			<header
				className="titlebar fixed top-0 left-0 right-0"
				data-tauri-drag-region
			/>

			<div className="w-full max-w-sm px-6">
				<form onSubmit={handleSubmit} className="space-y-4">
					<div className="text-center mb-8">
						<h1 className="text-lg font-medium text-foreground">
							Choose a username
						</h1>
						<p className="text-sm text-muted-foreground mt-1">
							This is how you'll appear to others
						</p>
					</div>

					<Input
						value={name}
						onChange={(e) => {
							setName(e.target.value);
							clearError();
						}}
						placeholder="your_username"
						disabled={isLoading}
						autoFocus
						className="text-center"
					/>

					<div className="min-h-[20px]">
						{name && name !== normalizedName && normalizedName && (
							<p className="text-xs text-muted-foreground text-center">
								Will be saved as:{" "}
								<code className="bg-muted px-1 rounded">{normalizedName}</code>
							</p>
						)}

						{name && !normalizedName && (
							<p className="text-xs text-destructive text-center">
								Username must contain at least one letter or number
							</p>
						)}

						{normalizedName && availability === "taken" && (
							<p className="text-xs text-destructive text-center">
								Username is already taken
							</p>
						)}

						{normalizedName && availability === "checking" && (
							<p className="text-xs text-muted-foreground text-center">
								Checking availability...
							</p>
						)}
					</div>

					{error && (
						<p className="text-sm text-destructive text-center">{error}</p>
					)}

					<Button type="submit" className="w-full" disabled={!canSubmit}>
						{isLoading ? "Saving..." : "Continue"}
					</Button>
				</form>
			</div>
		</div>
	);
}
