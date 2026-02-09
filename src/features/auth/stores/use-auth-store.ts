import type { Session, User } from "@supabase/supabase-js";
import { create } from "zustand";
import {
	fetchDisplayName,
	sendLoginCode,
	setDisplayName,
	signOut,
	supabase,
	verifyLoginCode,
} from "../lib/supabase";

type AuthState = {
	user: User | null;
	session: Session | null;
	displayName: string | null;
	needsUsername: boolean;
	email: string | null;
	isLoading: boolean;
	isInitialized: boolean;
	error: string | null;

	// Actions
	initialize: () => Promise<void>;
	sendCode: (email: string) => Promise<void>;
	verifyCode: (email: string, code: string) => Promise<void>;
	setUsername: (name: string) => Promise<void>;
	logout: () => Promise<void>;
	clearError: () => void;
};

export const useAuthStore = create<AuthState>((set, get) => ({
	user: null,
	session: null,
	displayName: null,
	needsUsername: false,
	email: null,
	isLoading: false,
	isInitialized: false,
	error: null,

	initialize: async () => {
		try {
			const {
				data: { session },
			} = await supabase.auth.getSession();

			if (session?.user) {
				const displayName = await fetchDisplayName(session.user.id);
				set({
					session,
					user: session.user,
					displayName,
					needsUsername: !displayName,
					email: session.user.email ?? null,
					isInitialized: true,
				});
			} else {
				set({
					session: null,
					user: null,
					isInitialized: true,
				});
			}

			// Listen for auth state changes (token refresh / sign out only).
			// SIGNED_IN and INITIAL_SESSION are handled by initialize() and
			// verifyCode() which also check display_name to avoid a flash.
			supabase.auth.onAuthStateChange((event, session) => {
				if (event === "TOKEN_REFRESHED") {
					set({ session, user: session?.user ?? null });
				} else if (event === "SIGNED_OUT") {
					set({
						session: null,
						user: null,
						displayName: null,
						needsUsername: false,
						email: null,
					});
				}
			});
		} catch (error) {
			set({
				isInitialized: true,
				error: error instanceof Error ? error.message : "Failed to initialize",
			});
		}
	},

	sendCode: async (email: string) => {
		set({ isLoading: true, error: null });
		try {
			await sendLoginCode(email);
			set({ isLoading: false });
		} catch (error) {
			set({
				isLoading: false,
				error: error instanceof Error ? error.message : "Failed to send code",
			});
			throw error;
		}
	},

	verifyCode: async (email: string, code: string) => {
		set({ isLoading: true, error: null });
		try {
			const session = await verifyLoginCode(email, code);
			if (session?.user) {
				const displayName = await fetchDisplayName(session.user.id);
				set({
					session,
					user: session.user,
					displayName,
					needsUsername: !displayName,
					email: session.user.email ?? email,
					isLoading: false,
				});
			} else {
				set({
					session,
					user: session?.user ?? null,
					isLoading: false,
				});
			}
		} catch (error) {
			set({
				isLoading: false,
				error: error instanceof Error ? error.message : "Invalid code",
			});
			throw error;
		}
	},

	setUsername: async (name: string) => {
		const { user } = get();
		if (!user) throw new Error("Not authenticated");

		set({ isLoading: true, error: null });
		try {
			await setDisplayName(user.id, name);
			set({
				displayName: name,
				needsUsername: false,
				isLoading: false,
			});
		} catch (error) {
			set({
				isLoading: false,
				error:
					error instanceof Error ? error.message : "Failed to set username",
			});
			throw error;
		}
	},

	logout: async () => {
		set({ isLoading: true, error: null });
		try {
			await signOut();
			set({
				session: null,
				user: null,
				displayName: null,
				needsUsername: false,
				email: null,
				isLoading: false,
			});
		} catch (error) {
			set({
				isLoading: false,
				error: error instanceof Error ? error.message : "Failed to sign out",
			});
		}
	},

	clearError: () => set({ error: null }),
}));
