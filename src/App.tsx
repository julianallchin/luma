import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow, Window } from "@tauri-apps/api/window";
import { ChevronLeft } from "lucide-react";
import { useEffect, useState } from "react";
import {
	HashRouter,
	Route,
	Routes,
	useLocation,
	useNavigate,
	useParams,
} from "react-router-dom";

import type { NodeTypeDef } from "./bindings/schema";
import type { Venue } from "./bindings/venues";
import "./App.css";
import { ThemeProvider } from "next-themes";
import { ProjectDashboard } from "./features/app/components/project-dashboard";
import { WelcomeScreen } from "./features/app/components/welcome-screen";
import { useAppViewStore } from "./features/app/stores/use-app-view-store";
import { LoginScreen } from "./features/auth/components/login-screen";
import { useAuthStore } from "./features/auth/stores/use-auth-store";
import { PatternEditor } from "./features/patterns/components/pattern-editor";
import { PerformPage } from "./features/perform/components/perform-page";
import { SettingsWindow } from "./features/settings/components/settings-window";
import { TrackEditor } from "./features/track-editor/components/track-editor";
import { useTrackEditorStore } from "./features/track-editor/stores/use-track-editor-store";
import { useTracksStore } from "./features/tracks/stores/use-tracks-store";
import { UniverseDesigner } from "./features/universe/components/universe-designer";
import { Toaster } from "./shared/components/ui/sonner";
import { cn } from "./shared/lib/utils";

// Wrapper for PatternEditor to extract params
function PatternEditorRoute({ nodeTypes }: { nodeTypes: NodeTypeDef[] }) {
	const { patternId } = useParams();
	return <PatternEditor patternId={Number(patternId)} nodeTypes={nodeTypes} />;
}

// Wrapper for TrackEditor to extract params
function TrackEditorRoute() {
	const { trackId } = useParams();
	const location = useLocation();
	const parsedTrackId = trackId ? Number(trackId) : null;
	const resolvedTrackId = Number.isNaN(parsedTrackId) ? null : parsedTrackId;
	const trackName =
		location.state?.trackName ||
		(resolvedTrackId !== null ? `Track ${resolvedTrackId}` : "");
	return <TrackEditor trackId={resolvedTrackId} trackName={trackName} />;
}

// Wrapper for UniverseDesigner to extract venue params and load venue
function UniverseDesignerRoute() {
	const { venueId } = useParams();
	const setVenue = useAppViewStore((state) => state.setVenue);
	const currentVenue = useAppViewStore((state) => state.currentVenue);

	useEffect(() => {
		if (!venueId) return;

		// Load venue data if not already loaded or different venue
		if (!currentVenue || currentVenue.id !== Number(venueId)) {
			invoke<Venue>("get_venue", { id: Number(venueId) })
				.then((venue) => {
					setVenue(venue);
				})
				.catch((err) => {
					console.error("Failed to load venue", err);
				});
		}
	}, [venueId, currentVenue, setVenue]);

	return <UniverseDesigner venueId={Number(venueId)} />;
}

// Wrapper for TrackEditor within venue context
function VenueTrackEditorRoute() {
	const { venueId } = useParams();
	const setVenue = useAppViewStore((state) => state.setVenue);
	const currentVenue = useAppViewStore((state) => state.currentVenue);

	useEffect(() => {
		if (!venueId) return;

		if (!currentVenue || currentVenue.id !== Number(venueId)) {
			invoke<Venue>("get_venue", { id: Number(venueId) })
				.then((venue) => {
					setVenue(venue);
				})
				.catch((err) => {
					console.error("Failed to load venue", err);
				});
		}
	}, [venueId, currentVenue, setVenue]);

	return <TrackEditor />;
}

// Wrapper for PerformPage within venue context
function VenuePerformRoute() {
	const { venueId } = useParams();
	const setVenue = useAppViewStore((state) => state.setVenue);
	const currentVenue = useAppViewStore((state) => state.currentVenue);

	useEffect(() => {
		if (!venueId) return;

		if (!currentVenue || currentVenue.id !== Number(venueId)) {
			invoke<Venue>("get_venue", { id: Number(venueId) })
				.then((venue) => {
					setVenue(venue);
				})
				.catch((err) => {
					console.error("Failed to load venue", err);
				});
		}
	}, [venueId, currentVenue, setVenue]);

	return <PerformPage />;
}

function MainApp() {
	const currentVenue = useAppViewStore((state) => state.currentVenue);
	const setVenue = useAppViewStore((state) => state.setVenue);
	const logout = useAuthStore((state) => state.logout);
	const activeTrackId = useTrackEditorStore((state) => state.trackId);
	const activeTrackName = useTrackEditorStore((state) => state.trackName);
	const tracks = useTracksStore((state) => state.tracks);

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

	const venueIdMatch = location.pathname.match(/^\/venue\/(\d+)/);
	const venueIdFromRoute = venueIdMatch ? Number(venueIdMatch[1]) : null;
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
				<div className="pl-20 flex items-center gap-3 justify-self-start">
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
								const isDisabled = false;
								return (
									<button
										key={tab.id}
										type="button"
										role="tab"
										aria-selected={isActive}
										disabled={isDisabled}
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
				{isTrackEditorRoute && (
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
					{currentVenue && (
						<button
							type="button"
							onClick={handleCloseVenue}
							className="text-xs opacity-50 hover:opacity-100 transition-opacity"
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
				<Routes>
					<Route path="/" element={<ProjectDashboard />} />
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
			</main>
		</div>
	);
}

function AuthGate({ children }: { children: React.ReactNode }) {
	const { user, isInitialized, initialize } = useAuthStore();

	useEffect(() => {
		initialize();
	}, [initialize]);

	// Show loading while checking auth state
	if (!isInitialized) {
		return (
			<div className="w-screen h-screen bg-background flex items-center justify-center">
				<header
					className="titlebar fixed top-0 left-0 right-0"
					data-tauri-drag-region
				/>
				<p className="text-sm text-muted-foreground">Loading...</p>
			</div>
		);
	}

	// Show login screen if not authenticated
	if (!user) {
		return <LoginScreen />;
	}

	// Show app if authenticated
	return <>{children}</>;
}

function App() {
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
		<HashRouter>
			<ThemeProvider attribute="class">
				<Toaster />
				<AuthGate>
					<Routes>
						<Route path="/*" element={<MainApp />} />
						<Route path="/settings" element={<SettingsWindow />} />
					</Routes>
				</AuthGate>
			</ThemeProvider>
		</HashRouter>
	);
}

export default App;
