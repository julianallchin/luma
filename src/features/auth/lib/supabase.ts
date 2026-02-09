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
 * Verify the OTP code and establish a session.
 * Profile row is created by the DB trigger; username is set separately.
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
 * Fetch display_name for a user. Returns null if not yet set.
 */
export async function fetchDisplayName(userId: string): Promise<string | null> {
	const { data } = await supabase
		.from("profiles")
		.select("display_name")
		.eq("id", userId)
		.single();
	return data?.display_name ?? null;
}

/**
 * Set the display_name for a user.
 */
export async function setDisplayName(
	userId: string,
	name: string,
): Promise<void> {
	const { error } = await supabase
		.from("profiles")
		.update({ display_name: name })
		.eq("id", userId);
	if (error) throw error;
}

/**
 * Check if a display_name is already taken by another user.
 */
export async function checkUsernameAvailable(name: string): Promise<boolean> {
	const { count } = await supabase
		.from("profiles")
		.select("id", { count: "exact", head: true })
		.eq("display_name", name);
	return count === 0;
}

/**
 * Sign out the current user
 */
export async function signOut(): Promise<void> {
	const { error } = await supabase.auth.signOut();
	if (error) throw error;
}
