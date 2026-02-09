import { useState } from "react";
import { Button } from "@/shared/components/ui/button";
import { Input } from "@/shared/components/ui/input";
import {
	InputOTP,
	InputOTPGroup,
	InputOTPSlot,
} from "@/shared/components/ui/input-otp";
import { useAuthStore } from "../stores/use-auth-store";

type Step = "email" | "otp";

export function LoginScreen() {
	const [step, setStep] = useState<Step>("email");
	const [email, setEmail] = useState("");
	const [code, setCode] = useState("");

	const { sendCode, verifyCode, isLoading, error, clearError } = useAuthStore();

	const handleEmailSubmit = async (e: React.FormEvent) => {
		e.preventDefault();
		if (!email.trim()) return;

		try {
			await sendCode(email.trim());
			setStep("otp");
		} catch {
			// Error is handled by the store
		}
	};

	const handleCodeComplete = async (value: string) => {
		setCode(value);
		if (value.length === 6) {
			try {
				await verifyCode(email, value);
			} catch {
				// Error is handled by the store
				setCode("");
			}
		}
	};

	const handleBackToEmail = () => {
		setStep("email");
		setCode("");
		clearError();
	};

	return (
		<div className="w-screen h-screen bg-background flex items-center justify-center">
			<header
				className="titlebar fixed top-0 left-0 right-0"
				data-tauri-drag-region
			/>

			<div className="w-full max-w-sm px-6">
				{step === "email" ? (
					<form onSubmit={handleEmailSubmit} className="space-y-4">
						<div className="text-center mb-8">
							<h1 className="text-lg font-medium text-foreground">
								Sign in to Luma
							</h1>
							<p className="text-sm text-muted-foreground mt-1">
								Enter your email to receive a code
							</p>
						</div>

						<Input
							type="email"
							placeholder="you@example.com"
							value={email}
							onChange={(e) => setEmail(e.target.value)}
							disabled={isLoading}
							autoFocus
							className="text-center"
						/>

						{error && (
							<p className="text-sm text-destructive text-center">{error}</p>
						)}

						<Button
							type="submit"
							className="w-full"
							disabled={isLoading || !email.trim()}
						>
							{isLoading ? "Sending..." : "Continue"}
						</Button>
					</form>
				) : (
					<div className="space-y-4">
						<div className="text-center mb-8">
							<h1 className="text-lg font-medium text-foreground">
								Check your email
							</h1>
							<p className="text-sm text-muted-foreground mt-1">
								Enter the 6-digit code sent to
							</p>
							<p className="text-sm font-medium text-foreground">{email}</p>
						</div>

						<div className="flex justify-center">
							<InputOTP
								maxLength={6}
								value={code}
								onChange={handleCodeComplete}
								disabled={isLoading}
								autoFocus
							>
								<InputOTPGroup>
									<InputOTPSlot index={0} />
									<InputOTPSlot index={1} />
									<InputOTPSlot index={2} />
									<InputOTPSlot index={3} />
									<InputOTPSlot index={4} />
									<InputOTPSlot index={5} />
								</InputOTPGroup>
							</InputOTP>
						</div>

						{error && (
							<p className="text-sm text-destructive text-center">{error}</p>
						)}

						{isLoading && (
							<p className="text-sm text-muted-foreground text-center">
								Verifying...
							</p>
						)}

						<button
							type="button"
							onClick={handleBackToEmail}
							className="w-full text-sm text-muted-foreground hover:text-foreground transition-colors"
							disabled={isLoading}
						>
							Use a different email
						</button>
					</div>
				)}
			</div>
		</div>
	);
}
