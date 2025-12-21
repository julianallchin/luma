import { createClient } from "@supabase/supabase-js";
import { invoke } from "@tauri-apps/api/core";

// These are public client-side credentials (safe to commit)
// Security is enforced via Supabase Row Level Security (RLS) policies
const SUPABASE_URL = "https://smuuycypmsutwrkpctws.supabase.co";
const SUPABASE_ANON_KEY = "sb_publishable_V8JRQkGliRYDAiGghjUrmQ_w8fpfjRb";

const SUPABASE_SESSION_KEY = "supabase_session";

const tauriStorage = {
	async getItem(_key: string): Promise<string | null> {
		return invoke<string | null>("get_session_item", {
			key: SUPABASE_SESSION_KEY,
		});
	},
	async setItem(_key: string, value: string): Promise<void> {
		await invoke("set_session_item", { key: SUPABASE_SESSION_KEY, value });
	},
	async removeItem(_key: string): Promise<void> {
		await invoke("remove_session_item", { key: SUPABASE_SESSION_KEY });
	},
};

export const supabase = createClient(SUPABASE_URL, SUPABASE_ANON_KEY, {
	auth: {
		persistSession: true,
		autoRefreshToken: true,
		detectSessionInUrl: false, // We use OTP codes, not redirect URLs
		storage: tauriStorage,
	},
});

/**
 * Send a 6-digit OTP code to the user's email
 */
export async function sendLoginCode(email: string): Promise<void> {
	const { error } = await supabase.auth.signInWithOtp({ email });
	if (error) throw error;
}

/**
 * Verify the OTP code and establish a session
 */
export async function verifyLoginCode(email: string, code: string) {
	const { data, error } = await supabase.auth.verifyOtp({
		email,
		token: code,
		type: "email",
	});
	if (error) throw error;
	return data.session;
}

/**
 * Sign out the current user
 */
export async function signOut(): Promise<void> {
	const { error } = await supabase.auth.signOut();
	if (error) throw error;
}
