import type { Session, User } from "@supabase/supabase-js";
import { create } from "zustand";
import {
	sendLoginCode,
	signOut,
	supabase,
	verifyLoginCode,
} from "../lib/supabase";

type AuthState = {
	user: User | null;
	session: Session | null;
	isLoading: boolean;
	isInitialized: boolean;
	error: string | null;

	// Actions
	initialize: () => Promise<void>;
	sendCode: (email: string) => Promise<void>;
	verifyCode: (email: string, code: string) => Promise<void>;
	logout: () => Promise<void>;
	clearError: () => void;
};

export const useAuthStore = create<AuthState>((set) => ({
	user: null,
	session: null,
	isLoading: false,
	isInitialized: false,
	error: null,

	initialize: async () => {
		try {
			const {
				data: { session },
			} = await supabase.auth.getSession();
			set({
				session,
				user: session?.user ?? null,
				isInitialized: true,
			});

			// Listen for auth state changes
			supabase.auth.onAuthStateChange((_event, session) => {
				set({
					session,
					user: session?.user ?? null,
				});
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
			set({
				session,
				user: session?.user ?? null,
				isLoading: false,
			});
		} catch (error) {
			set({
				isLoading: false,
				error: error instanceof Error ? error.message : "Invalid code",
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
