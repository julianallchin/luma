import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow, Window } from "@tauri-apps/api/window";
import { ChevronLeft } from "lucide-react";
import { lazy, Suspense, useEffect, useState } from "react";
import {
	createHashRouter,
	Outlet,
	Route,
	RouterProvider,
	Routes,
	useLocation,
	useNavigate,
	useParams,
} from "react-router-dom";
import { toast } from "sonner";

import type { NodeTypeDef } from "./bindings/schema";
import type { Venue } from "./bindings/venues";
import { WelcomeScreen } from "./features/app/components/welcome-screen";
import { useAppViewStore } from "./features/app/stores/use-app-view-store";
import { LoginScreen } from "./features/auth/components/login-screen";
import { UsernameScreen } from "./features/auth/components/username-screen";
import { useAuthStore } from "./features/auth/stores/use-auth-store";
import { usePatternsStore } from "./features/patterns/stores/use-patterns-store";
import { useTrackEditorStore } from "./features/track-editor/stores/use-track-editor-store";
import { useTracksStore } from "./features/tracks/stores/use-tracks-store";
import { useFixtureStore } from "./features/universe/stores/use-fixture-store";
import { ShareVenueDialog } from "./features/venues/components/share-venue-dialog";
import { useVenuesStore } from "./features/venues/stores/use-venues-store";
import { ErrorBoundary } from "./shared/components/error-boundary";
import { Toaster } from "./shared/components/ui/sonner";
import { cn } from "./shared/lib/utils";
import "./App.css";

const PatternEditor = lazy(() =>
	import("./features/patterns/components/pattern-editor").then((m) => ({
		default: m.PatternEditor,
	})),
);
const TrackEditor = lazy(() =>
	import("./features/track-editor/components/track-editor").then((m) => ({
		default: m.TrackEditor,
	})),
);
const PerformPage = lazy(() =>
	import("./features/perform/components/perform-page").then((m) => ({
		default: m.PerformPage,
	})),
);
const UniverseDesigner = lazy(() =>
	import("./features/universe/components/universe-designer").then((m) => ({
		default: m.UniverseDesigner,
	})),
);
const SettingsWindow = lazy(() =>
	import("./features/settings/components/settings-window").then((m) => ({
		default: m.SettingsWindow,
	})),
);

// Wrapper for PatternEditor to extract params
function PatternEditorRoute({ nodeTypes }: { nodeTypes: NodeTypeDef[] }) {
	const { patternId } = useParams();
	if (!patternId) return null;
	return <PatternEditor patternId={patternId} nodeTypes={nodeTypes} />;
}

// Wrapper for TrackEditor to extract params
function TrackEditorRoute() {
	const { trackId } = useParams();
	const location = useLocation();
	const resolvedTrackId = trackId ?? null;
	const trackName =
		location.state?.trackName ||
		(resolvedTrackId !== null ? `Track ${resolvedTrackId}` : "");
	return <TrackEditor trackId={resolvedTrackId} trackName={trackName} />;
}

// Wrapper for UniverseDesigner to extract venue params and load venue
function UniverseDesignerRoute() {
	const { venueId } = useParams();
	const setVenue = useAppViewStore((state) => state.setVenue);

	useEffect(() => {
		if (!venueId) return;
		// Always re-fetch venue to get current role (may change across sessions)
		invoke<Venue>("get_venue", { id: venueId })
			.then((venue) => setVenue(venue))
			.catch((err) => console.error("Failed to load venue", err));
	}, [venueId, setVenue]);

	if (!venueId) return null;
	return <UniverseDesigner venueId={venueId} />;
}

// Wrapper for TrackEditor within venue context
function VenueTrackEditorRoute() {
	const { venueId } = useParams();
	const setVenue = useAppViewStore((state) => state.setVenue);

	useEffect(() => {
		if (!venueId) return;
		invoke<Venue>("get_venue", { id: venueId })
			.then((venue) => setVenue(venue))
			.catch((err) => console.error("Failed to load venue", err));
	}, [venueId, setVenue]);

	return <TrackEditor />;
}

// Wrapper for PerformPage within venue context
function VenuePerformRoute() {
	const { venueId } = useParams();
	const setVenue = useAppViewStore((state) => state.setVenue);

	useEffect(() => {
		if (!venueId) return;
		invoke<Venue>("get_venue", { id: venueId })
			.then((venue) => setVenue(venue))
			.catch((err) => console.error("Failed to load venue", err));
	}, [venueId, setVenue]);

	return <PerformPage />;
}

const isMac = navigator.platform.startsWith("Mac");

function MainApp() {
	const currentVenue = useAppViewStore((state) => state.currentVenue);
	const setVenue = useAppViewStore((state) => state.setVenue);
	const logout = useAuthStore((state) => state.logout);
	const activeTrackId = useTrackEditorStore((state) => state.trackId);
	const activeTrackName = useTrackEditorStore((state) => state.trackName);
	const tracks = useTracksStore((state) => state.tracks);
	const ungroupedCount = useFixtureStore(
		(state) => state.ungroupedFixtures.length,
	);

	const navigate = useNavigate();
	const location = useLocation();

	const [nodeTypes, setNodeTypes] = useState<NodeTypeDef[]>([]);
	const isPatternRoute = location.pathname.startsWith("/pattern/");
	const patternBackLabel = (location.state as { backLabel?: string } | null)
		?.backLabel;
	const isTrackEditorRoute =
		location.pathname.startsWith("/track/") ||
		(location.pathname.includes("/venue/") &&
			location.pathname.includes("/edit"));
	const activeTrack =
		tracks.find((track) => track.id === activeTrackId) ?? null;
	const trackTitle =
		activeTrack?.title ||
		activeTrack?.filePath?.split("/").pop() ||
		activeTrackName ||
		(activeTrackId !== null ? `Track ${activeTrackId}` : "No track selected");
	const trackArtist = activeTrack?.artist ?? "";
	const trackArt = activeTrack?.albumArtData ?? null;
	const handlePatternBack = () => {
		const from = (location.state as { from?: string } | null)?.from;
		if (from) {
			navigate(from);
			return;
		}
		if (window.history.length > 1) {
			navigate(-1);
			return;
		}
		navigate("/");
	};

	// Load node types only when needed (in pattern editor)
	useEffect(() => {
		// Simple check if we are in a pattern route
		if (!isPatternRoute) return;

		let active = true;
		invoke<NodeTypeDef[]>("get_node_types")
			.then((types) => {
				if (!active) return;
				setNodeTypes(types);
			})
			.catch((err) => {
				console.error("Failed to fetch node catalog", err);
			});

		return () => {
			active = false;
		};
	}, [isPatternRoute, location.pathname]);

	const handleCloseVenue = () => {
		setVenue(null);
		navigate("/");
	};

	const venueIdMatch = location.pathname.match(/^\/venue\/([^/]+)/);
	const venueIdFromRoute = venueIdMatch ? venueIdMatch[1] : null;
	const venueIdForTabs = currentVenue?.id ?? venueIdFromRoute;
	const showVenueTabs = Boolean(venueIdFromRoute);
	const activeVenueTab = location.pathname.includes("/edit")
		? "edit"
		: location.pathname.includes("/perform")
			? "perform"
			: location.pathname.includes("/universe")
				? "universe"
				: null;

	// Check if we're on a venue route
	const isVenueRoute = location.pathname.startsWith("/venue/");
	const isWelcomeScreen = location.pathname === "/" && !isVenueRoute;

	// Show welcome screen at root
	if (isWelcomeScreen) {
		return (
			<div className="w-screen h-screen bg-background">
				<header className="titlebar" data-tauri-drag-region />
				<div className="pt-titlebar w-full h-full">
					<WelcomeScreen />
				</div>
			</div>
		);
	}

	return (
		<div className="w-screen h-screen bg-background">
			<header
				className="titlebar titlebar-grid grid-cols-[minmax(0,1fr)_auto_minmax(0,1fr)] items-center pr-4"
				data-tauri-drag-region
			>
				<div
					className={cn(
						"flex items-center gap-3 justify-self-start",
						isMac ? "pl-20" : "pl-4",
					)}
				>
					{isPatternRoute && (
						<button
							type="button"
							onClick={handlePatternBack}
							className="no-drag flex items-center gap-1 text-muted-foreground hover:text-foreground transition-colors text-xs max-w-[40vw]"
							aria-label="Back"
						>
							<ChevronLeft className="h-4 w-4" />
							<span className="truncate">
								{patternBackLabel ? `back to ${patternBackLabel}` : "back"}
							</span>
						</button>
					)}
					{showVenueTabs && venueIdForTabs !== null && (
						<div
							className="no-drag flex items-center border border-border/60 bg-background/70 p-0.5 text-xs font-medium backdrop-blur-sm"
							role="tablist"
							aria-label="Venue view"
						>
							{(
								[
									{ id: "universe", label: "Universe" },
									{ id: "edit", label: "Edit" },
									{ id: "perform", label: "Perform" },
								] as const
							).map((tab) => {
								const isActive = activeVenueTab === tab.id;
								// Block leaving universe tab when fixtures are ungrouped
								const isBlocked =
									activeVenueTab === "universe" &&
									tab.id !== "universe" &&
									ungroupedCount > 0;
								const isDisabled = isBlocked;
								return (
									<button
										key={tab.id}
										type="button"
										role="tab"
										aria-selected={isActive}
										disabled={isDisabled}
										title={
											isBlocked
												? `${ungroupedCount} fixture${ungroupedCount !== 1 ? "s" : ""} need a group`
												: undefined
										}
										onClick={() => {
											if (isDisabled) return;
											navigate(`/venue/${venueIdForTabs}/${tab.id}`);
										}}
										className={cn(
											"px-3 py-1 transition-colors",
											isActive
												? "bg-foreground text-background"
												: "text-muted-foreground hover:text-foreground",
											isDisabled && "cursor-not-allowed opacity-40",
										)}
									>
										{tab.label}
									</button>
								);
							})}
						</div>
					)}
				</div>
				{isTrackEditorRoute && activeTrackId !== null && (
					<div className="flex items-center justify-center min-w-0 justify-self-center col-start-2">
						<div className="flex items-center gap-2 min-w-0">
							<div className="relative h-7 w-7 overflow-hidden rounded bg-muted/50 flex-shrink-0">
								{trackArt ? (
									<img
										src={trackArt}
										alt=""
										className="h-full w-full object-cover"
									/>
								) : (
									<div className="w-full h-full flex items-center justify-center bg-muted text-[7px] text-muted-foreground uppercase tracking-tighter">
										No Art
									</div>
								)}
							</div>
							<div className="min-w-0">
								<div className="text-xs font-medium text-foreground/90 truncate leading-tight">
									{trackTitle}
								</div>
								{trackArtist ? (
									<div className="text-[10px] text-muted-foreground truncate leading-tight">
										{trackArtist}
									</div>
								) : null}
							</div>
						</div>
					</div>
				)}
				<div className="no-drag flex items-center gap-4 justify-self-end col-start-3">
					{currentVenue && currentVenue.role === "member" && (
						<span className="text-[9px] px-1.5 py-0.5 rounded bg-muted-foreground/10 text-muted-foreground">
							joined
						</span>
					)}
					{currentVenue && currentVenue.role === "owner" && (
						<ShareVenueDialog
							venueId={currentVenue.id}
							existingCode={currentVenue.shareCode}
						/>
					)}
					{currentVenue && (
						<button
							type="button"
							onClick={handleCloseVenue}
							disabled={activeVenueTab === "universe" && ungroupedCount > 0}
							title={
								activeVenueTab === "universe" && ungroupedCount > 0
									? `${ungroupedCount} fixture${ungroupedCount !== 1 ? "s" : ""} need a group`
									: undefined
							}
							className={cn(
								"text-xs opacity-50 hover:opacity-100 transition-opacity",
								activeVenueTab === "universe" &&
									ungroupedCount > 0 &&
									"cursor-not-allowed opacity-30 hover:opacity-30",
							)}
						>
							[ close venue ]
						</button>
					)}
					<button
						type="button"
						onClick={logout}
						className="text-xs opacity-50 hover:opacity-100 transition-opacity"
					>
						[ sign out ]
					</button>
				</div>
			</header>

			<main className="pt-titlebar w-full h-full">
				<Suspense
					fallback={<div className="w-screen h-screen bg-background" />}
				>
					<Routes>
						<Route
							path="/pattern/:patternId"
							element={<PatternEditorRoute nodeTypes={nodeTypes} />}
						/>
						<Route path="/track/:trackId" element={<TrackEditorRoute />} />
						<Route
							path="/venue/:venueId/edit"
							element={<VenueTrackEditorRoute />}
						/>
						<Route
							path="/venue/:venueId/universe"
							element={<UniverseDesignerRoute />}
						/>
						<Route
							path="/venue/:venueId/perform"
							element={<VenuePerformRoute />}
						/>
						{/* Keep legacy route for backwards compatibility */}
						<Route path="/universe" element={<UniverseDesigner />} />
					</Routes>
				</Suspense>
			</main>
		</div>
	);
}

// Track sync state — module-level so it survives strict mode remounts
let syncingForUserId: string | null = null;

function AuthGate({ children }: { children: React.ReactNode }) {
	const { user, isInitialized, needsUsername } = useAuthStore();

	// Full sync when authenticated (discovery → pull → push → files)
	useEffect(() => {
		if (user && syncingForUserId !== user.id) {
			syncingForUserId = user.id;
			usePatternsStore.getState().setCurrentUserId(user.id);

			// Single coordinated sync call — handles everything:
			// 1. Discovers venues (owned + joined) from Supabase
			// 2. Delta-pulls all entities modified since last sync
			// 3. Pushes dirty local records
			// 4. Then syncs files (audio + stems)
			invoke("sync_full")
				.then((report) => {
					console.log("[sync] Full sync complete:", report);
					// Refresh stores after sync pulls new data
					usePatternsStore.getState().pullOwn();
					usePatternsStore.getState().pullCommunity();
					useVenuesStore.getState().refresh();
				})
				.catch((err) => console.error("[sync] Full sync failed:", err));
		}
	}, [user?.id]);

	// Show empty screen while checking auth state — the dark background
	// from index.html makes this invisible so there's no flash.
	if (!isInitialized) {
		return (
			<div className="w-screen h-screen bg-background">
				<header
					className="titlebar fixed top-0 left-0 right-0"
					data-tauri-drag-region
				/>
			</div>
		);
	}

	// Show login screen if not authenticated
	if (!user) {
		return <LoginScreen />;
	}

	// Show username screen if display_name not yet set
	if (needsUsername) {
		return <UsernameScreen />;
	}

	// Show app if authenticated
	return <>{children}</>;
}

function AppLayout() {
	// Track Python environment setup progress via backend events
	useEffect(() => {
		const toastId = "python-env";
		const unlisten = listen<[string, string]>(
			"python-env-progress",
			(event) => {
				const [status, message] = event.payload;
				if (status === "setup") {
					toast.loading(message, { id: toastId });
				} else if (status === "ready") {
					toast.success(message, { id: toastId });
				} else if (status === "error") {
					toast.error(message, { id: toastId });
				}
			},
		);
		return () => {
			unlisten.then((f) => f());
		};
	}, []);

	// Global keyboard shortcut for settings (Ctrl+, on Linux/Windows, Cmd+, on macOS)
	useEffect(() => {
		const handleKeyDown = async (e: KeyboardEvent) => {
			if (e.key === "," && (e.ctrlKey || e.metaKey)) {
				e.preventDefault();
				// Don't open settings from the settings window itself
				const currentWindow = getCurrentWindow();
				if (currentWindow.label === "settings") return;

				const settingsWindow = new Window("settings");
				await settingsWindow.show();
				await settingsWindow.setFocus();
			}
		};

		window.addEventListener("keydown", handleKeyDown);
		return () => window.removeEventListener("keydown", handleKeyDown);
	}, []);

	return (
		<>
			<Toaster />
			<ErrorBoundary>
				<AuthGate>
					<Outlet />
				</AuthGate>
			</ErrorBoundary>
		</>
	);
}

const router = createHashRouter([
	{
		element: <AppLayout />,
		children: [
			{ path: "/*", element: <MainApp /> },
			{
				path: "/settings",
				element: (
					<Suspense
						fallback={<div className="w-screen h-screen bg-background" />}
					>
						<SettingsWindow />
					</Suspense>
				),
			},
		],
	},
]);

function App() {
	return <RouterProvider router={router} />;
}

export default App;
