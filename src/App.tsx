import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { relaunch } from "@tauri-apps/plugin-process";
import { check } from "@tauri-apps/plugin-updater";
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
import { useShallow } from "zustand/react/shallow";

import type { NodeTypeDef } from "./bindings/schema";
import type { Venue } from "./bindings/venues";
import { UploadProgressBar } from "./features/app/components/upload-progress-bar";
import { WelcomeScreen } from "./features/app/components/welcome-screen";
import { useAppViewStore } from "./features/app/stores/use-app-view-store";
import { useUploadProgressStore } from "./features/app/stores/use-upload-progress-store";
import { LoginScreen } from "./features/auth/components/login-screen";
import { UsernameScreen } from "./features/auth/components/username-screen";
import { useAuthStore } from "./features/auth/stores/use-auth-store";
import { usePatternsStore } from "./features/patterns/stores/use-patterns-store";
import { SettingsDialog } from "./features/settings/components/settings-dialog";
import { useSettingsDialogStore } from "./features/settings/stores/use-settings-dialog-store";
import { useTrackEditorStore } from "./features/track-editor/stores/use-track-editor-store";
import { useTracksStore } from "./features/tracks/stores/use-tracks-store";
import { useFixtureStore } from "./features/universe/stores/use-fixture-store";
import { ShareVenueDialog } from "./features/venues/components/share-venue-dialog";
import { useVenuesStore } from "./features/venues/stores/use-venues-store";
import { ErrorBoundary } from "./shared/components/error-boundary";
import { HeaderActions } from "./shared/components/header-actions";
import {
	AlertDialog,
	AlertDialogAction,
	AlertDialogCancel,
	AlertDialogContent,
	AlertDialogDescription,
	AlertDialogFooter,
	AlertDialogHeader,
	AlertDialogTitle,
} from "./shared/components/ui/alert-dialog";
import { Button } from "./shared/components/ui/button";
import { Dropdown } from "./shared/components/ui/dropdown";
import { Selector } from "./shared/components/ui/selector";
import { Toaster } from "./shared/components/ui/sonner";
import { WindowControls } from "./shared/components/window-controls";
import "./App.css";

// Lazy chunks. We hold onto the raw import promises so we can prefetch
// them at idle — switching venue tabs (Universe/Edit/Perform) shouldn't
// pay for a JS chunk download on first navigation.
const importPatternEditor = () =>
	import("./features/patterns/components/pattern-editor");
const importTrackEditor = () =>
	import("./features/track-editor/components/track-editor");
const importPerformPage = () =>
	import("./features/perform/components/perform-page");
const importUniverseDesigner = () =>
	import("./features/universe/components/universe-designer");

const PatternEditor = lazy(() =>
	importPatternEditor().then((m) => ({ default: m.PatternEditor })),
);
const TrackEditor = lazy(() =>
	importTrackEditor().then((m) => ({ default: m.TrackEditor })),
);
const PerformPage = lazy(() =>
	importPerformPage().then((m) => ({ default: m.PerformPage })),
);
const UniverseDesigner = lazy(() =>
	importUniverseDesigner().then((m) => ({ default: m.UniverseDesigner })),
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

/// Titlebar pill showing the current track's art + title + artist. Extracted
/// so the `useTracksStore` subscription only mounts when the pill is visible
/// — and uses `useShallow` so it only re-renders when this track's title /
/// artist / art changes, not on every unrelated mutation to the tracks array.
function ActiveTrackPill({ activeTrackId }: { activeTrackId: string }) {
	const { title, artist, albumArtPath, filePath } = useTracksStore(
		useShallow((state) => {
			const t = state.tracks.find((track) => track.id === activeTrackId);
			return {
				title: t?.title ?? null,
				artist: t?.artist ?? null,
				albumArtPath: t?.albumArtPath ?? null,
				filePath: t?.filePath ?? null,
			};
		}),
	);
	const fallbackName = useTrackEditorStore((s) => s.trackName);
	const trackTitle =
		title ||
		filePath?.split("/").pop() ||
		fallbackName ||
		`Track ${activeTrackId}`;
	const trackArt = albumArtPath ? convertFileSrc(albumArtPath) : null;
	return (
		<div className="flex items-center justify-center min-w-0 justify-self-center col-start-2">
			<div className="flex items-center gap-2 min-w-0">
				<div className="relative h-7 w-7 overflow-hidden rounded bg-muted/50 flex-shrink-0">
					{trackArt ? (
						<img src={trackArt} alt="" className="h-full w-full object-cover" />
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
					{artist ? (
						<div className="text-[10px] text-muted-foreground truncate leading-tight">
							{artist}
						</div>
					) : null}
				</div>
			</div>
		</div>
	);
}

function MainApp() {
	const currentVenue = useAppViewStore((state) => state.currentVenue);
	const setVenue = useAppViewStore((state) => state.setVenue);
	const activeTrackId = useTrackEditorStore((state) => state.trackId);
	const ungroupedCount = useFixtureStore(
		(state) => state.ungroupedFixtures.length,
	);

	const navigate = useNavigate();
	const location = useLocation();

	const [nodeTypes, setNodeTypes] = useState<NodeTypeDef[]>([]);
	const [shareDialogOpen, setShareDialogOpen] = useState(false);
	const isPatternRoute = location.pathname.startsWith("/pattern/");
	const patternBackLabel = (location.state as { backLabel?: string } | null)
		?.backLabel;
	const isTrackEditorRoute =
		location.pathname.startsWith("/track/") ||
		(location.pathname.includes("/venue/") &&
			location.pathname.includes("/edit"));
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

	// Clear stale currentVenue only after the route has actually left the
	// venue — clearing it during the click handler would flush a render where
	// the dropdown is gone but the welcome screen hasn't mounted yet.
	useEffect(() => {
		if (!isVenueRoute && currentVenue) {
			setVenue(null);
		}
	}, [isVenueRoute, currentVenue, setVenue]);

	// Show welcome screen at root
	if (isWelcomeScreen) {
		return (
			<div className="w-screen h-screen bg-background flex flex-col">
				<header className="titlebar justify-end" data-tauri-drag-region>
					<HeaderActions />
				</header>
				<div className="w-full flex-1 min-h-0">
					<WelcomeScreen />
				</div>
			</div>
		);
	}

	return (
		<div className="w-screen h-screen bg-background flex flex-col">
			<header
				className="titlebar titlebar-grid grid-cols-[minmax(0,1fr)_auto_minmax(0,1fr)] items-center"
				data-tauri-drag-region
			>
				<div className="flex items-center gap-3 justify-self-start pl-0">
					{isPatternRoute && (
						<Button
							onClick={handlePatternBack}
							className="no-drag max-w-[40vw]"
							aria-label="Back"
						>
							<ChevronLeft />
							<span className="truncate">
								{patternBackLabel ? `back to ${patternBackLabel}` : "back"}
							</span>
						</Button>
					)}
					{showVenueTabs && venueIdForTabs !== null && (
						<div className="no-drag">
							<Selector
								value={activeVenueTab}
								onChange={(next) =>
									navigate(`/venue/${venueIdForTabs}/${next}`)
								}
								align="start"
								hideSelected
								options={(
									[
										{ value: "universe", label: "Universe" },
										{ value: "edit", label: "Edit" },
										{ value: "perform", label: "Perform" },
									] as const
								).map((tab) => ({
									value: tab.value,
									label: tab.label,
									// Block leaving universe tab while fixtures are ungrouped
									disabled:
										activeVenueTab === "universe" &&
										tab.value !== "universe" &&
										ungroupedCount > 0,
								}))}
							/>
						</div>
					)}
				</div>
				{isTrackEditorRoute && activeTrackId !== null && (
					<ActiveTrackPill activeTrackId={activeTrackId} />
				)}
				<div className="no-drag flex items-center gap-2 justify-self-end col-start-3">
					{currentVenue && currentVenue.role === "member" && (
						<span className="text-[9px] px-1.5 py-0.5 rounded bg-muted-foreground/10 text-muted-foreground">
							joined
						</span>
					)}
					{currentVenue && (
						<Dropdown
							label={
								<span className="truncate max-w-[200px]">
									{currentVenue.name}
								</span>
							}
							items={[
								...(currentVenue.role === "owner"
									? [
											{
												label: "Share",
												onClick: () => setShareDialogOpen(true),
											},
										]
									: []),
								{
									label: "Close",
									onClick: handleCloseVenue,
									disabled: activeVenueTab === "universe" && ungroupedCount > 0,
								},
							]}
						/>
					)}
					<HeaderActions />
				</div>
			</header>

			<main className="w-full flex-1 min-h-0">
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
			<UploadProgressBar />
			{currentVenue && currentVenue.role === "owner" && (
				<ShareVenueDialog
					venueId={currentVenue.id}
					existingCode={currentVenue.shareCode}
					open={shareDialogOpen}
					onOpenChange={setShareDialogOpen}
				/>
			)}
		</div>
	);
}

// Track sync state — module-level so it survives strict mode remounts AND HMR
let syncingForUserId: string | null = null;
let syncInFlight = false;

function AuthGate({ children }: { children: React.ReactNode }) {
	const user = useAuthStore((s) => s.user);
	const isInitialized = useAuthStore((s) => s.isInitialized);
	const needsUsername = useAuthStore((s) => s.needsUsername);
	const [showCloseDialog, setShowCloseDialog] = useState(false);

	// Upload progress events — accumulate across sync cycles this session.
	useEffect(() => {
		const unlistenStart = listen<{ count: number }>(
			"upload-progress-start",
			(e) => useUploadProgressStore.getState().addToTotal(e.payload.count),
		);
		const unlistenTick = listen("upload-progress-tick", () =>
			useUploadProgressStore.getState().tick(),
		);
		return () => {
			unlistenStart.then((f) => f());
			unlistenTick.then((f) => f());
		};
	}, []);

	// Intercept window close — confirm if uploads are in progress.
	useEffect(() => {
		const unlisten = listen("close-requested", () => {
			const { total, completed } = useUploadProgressStore.getState();
			if (total > 0 && completed < total) {
				setShowCloseDialog(true);
			} else {
				invoke("force_quit");
			}
		});
		return () => {
			unlisten.then((f) => f());
		};
	}, []);

	// Refresh stores whenever the backend signals new data (emitted after pull,
	// well before file sync finishes so the UI updates immediately).
	useEffect(() => {
		const unlisten = listen("library-changed", () => {
			usePatternsStore.getState().refresh();
			useVenuesStore.getState().refresh();
		});
		return () => {
			unlisten.then((f) => f());
		};
	}, []);

	// Full sync when authenticated (discovery → pull → push → files)
	useEffect(() => {
		if (user && !syncInFlight && syncingForUserId !== user.id) {
			syncingForUserId = user.id;
			syncInFlight = true;
			usePatternsStore.getState().setCurrentUserId(user.id);

			invoke("sync_full")
				.then((report) => console.log("[sync] Full sync complete:", report))
				.catch((err) => console.error("[sync] Full sync failed:", err))
				.finally(() => {
					syncInFlight = false;
				});
		}
	}, [user?.id]);

	// Show empty screen while checking auth state — the dark background
	// from index.html makes this invisible so there's no flash.
	if (!isInitialized) {
		return (
			<div className="w-screen h-screen bg-background">
				<header
					className="titlebar fixed top-0 left-0 right-0 justify-end"
					data-tauri-drag-region
				>
					<WindowControls />
				</header>
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
	return (
		<>
			{children}
			<AlertDialog open={showCloseDialog} onOpenChange={setShowCloseDialog}>
				<AlertDialogContent>
					<AlertDialogHeader>
						<AlertDialogTitle>Uploads in progress</AlertDialogTitle>
						<AlertDialogDescription>
							Files are still uploading. If you quit now, they'll resume next
							time you open Luma.
						</AlertDialogDescription>
					</AlertDialogHeader>
					<AlertDialogFooter>
						<AlertDialogCancel>Keep waiting</AlertDialogCancel>
						<AlertDialogAction onClick={() => invoke("force_quit")}>
							Quit anyway
						</AlertDialogAction>
					</AlertDialogFooter>
				</AlertDialogContent>
			</AlertDialog>
		</>
	);
}

function AppLayout() {
	// Prefetch all venue-tab route chunks once the app is idle so switching
	// between Universe / Edit / Perform doesn't pay for a JS chunk download
	// on first navigation. requestIdleCallback in browsers that support it,
	// setTimeout fallback otherwise.
	useEffect(() => {
		const prefetch = () => {
			void importTrackEditor();
			void importUniverseDesigner();
			void importPerformPage();
			void importPatternEditor();
		};
		const ric =
			typeof window !== "undefined" &&
			"requestIdleCallback" in window &&
			(
				window as unknown as {
					requestIdleCallback?: (cb: () => void) => number;
				}
			).requestIdleCallback;
		if (ric) {
			ric(prefetch);
		} else {
			const t = setTimeout(prefetch, 800);
			return () => clearTimeout(t);
		}
	}, []);

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

	// Background auto-updater: check on launch, then every 2 hours
	useEffect(() => {
		const TWO_HOURS = 2 * 60 * 60 * 1000;

		const checkForUpdate = async () => {
			try {
				const update = await check();
				if (!update) return;
				// Download + install in background (replaces app bundle on macOS)
				await update.downloadAndInstall();
				// Show persistent toast prompting relaunch
				toast("Update ready", {
					description: `Luma v${update.version} has been downloaded.`,
					duration: Number.POSITIVE_INFINITY,
					dismissible: false,
					action: {
						label: "Restart",
						onClick: () => relaunch(),
					},
				});
			} catch (e) {
				console.warn("[updater]", e);
			}
		};

		// Initial check shortly after launch (let the app settle first)
		const timeout = setTimeout(checkForUpdate, 5_000);
		const interval = setInterval(checkForUpdate, TWO_HOURS);
		return () => {
			clearTimeout(timeout);
			clearInterval(interval);
		};
	}, []);

	// Global keyboard shortcut for settings (Ctrl+, on Linux/Windows, Cmd+, on macOS)
	// and the corresponding macOS menu event emitted by the Rust backend.
	useEffect(() => {
		const toggle = useSettingsDialogStore.getState().toggle;
		const open = () => useSettingsDialogStore.getState().setOpen(true);

		const handleKeyDown = (e: KeyboardEvent) => {
			if (e.key === "," && (e.ctrlKey || e.metaKey)) {
				e.preventDefault();
				toggle();
			}
		};
		window.addEventListener("keydown", handleKeyDown);

		const unlisten = listen("open-settings", open);

		return () => {
			window.removeEventListener("keydown", handleKeyDown);
			unlisten.then((f) => f());
		};
	}, []);

	return (
		<>
			<Toaster />
			<SettingsDialog />
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
		children: [{ path: "/*", element: <MainApp /> }],
	},
]);

function App() {
	return <RouterProvider router={router} />;
}

export default App;
