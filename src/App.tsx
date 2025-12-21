import { invoke } from "@tauri-apps/api/core";
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
import "./App.css";
import { ProjectDashboard } from "./features/app/components/project-dashboard";
import { WelcomeScreen } from "./features/app/components/welcome-screen";
import { useAppViewStore } from "./features/app/stores/use-app-view-store";
import { LoginScreen } from "./features/auth/components/login-screen";
import { useAuthStore } from "./features/auth/stores/use-auth-store";
import { PatternEditor } from "./features/patterns/components/pattern-editor";
import { SettingsWindow } from "./features/settings/components/settings-window";
import { TrackEditor } from "./features/track-editor/components/track-editor";
import { UniverseDesigner } from "./features/universe/components/universe-designer";
import { Toaster } from "./shared/components/ui/sonner";

// Wrapper for PatternEditor to extract params
function PatternEditorRoute({ nodeTypes }: { nodeTypes: NodeTypeDef[] }) {
	const { patternId } = useParams();
	return <PatternEditor patternId={Number(patternId)} nodeTypes={nodeTypes} />;
}

// Wrapper for TrackEditor to extract params
function TrackEditorRoute() {
	const { trackId } = useParams();
	const location = useLocation();
	const trackName = location.state?.trackName || `Track ${trackId}`;
	return <TrackEditor trackId={Number(trackId)} trackName={trackName} />;
}

function MainApp() {
	const currentProject = useAppViewStore((state) => state.currentProject);
	const setProject = useAppViewStore((state) => state.setProject);
	const logout = useAuthStore((state) => state.logout);

	const navigate = useNavigate();
	const location = useLocation();

	const [nodeTypes, setNodeTypes] = useState<NodeTypeDef[]>([]);

	// Load node types only when needed (in pattern editor)
	useEffect(() => {
		// Simple check if we are in a pattern route
		if (!location.pathname.startsWith("/pattern/")) return;

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
	}, [location.pathname]);

	const handleCloseProject = async () => {
		try {
			await invoke("close_project");
			setProject(null);
			navigate("/");
		} catch (e) {
			console.error("Failed to close project", e);
		}
	};

	if (!currentProject) {
		return (
			<div className="w-screen h-screen bg-background">
				<header className="titlebar" data-tauri-drag-region />
				<div className="pt-titlebar w-full h-full">
					<WelcomeScreen />
				</div>
			</div>
		);
	}

	// Determine title based on route
	let title = currentProject.name;
	let showBack = false;

	if (location.pathname.startsWith("/pattern/")) {
		title = location.state?.name || "Pattern Editor";
		showBack = true;
	} else if (location.pathname.startsWith("/track/")) {
		title = location.state?.trackName || "Track Editor";
		showBack = true;
	} else if (location.pathname === "/universe") {
		title = "Universe Designer";
		showBack = true;
	}

	return (
		<div className="w-screen h-screen bg-background">
			<header
				className="titlebar flex justify-between items-center pr-4"
				data-tauri-drag-region
			>
				<div className="pl-16 flex items-center gap-3">
					{showBack && (
						<button
							type="button"
							onClick={() => navigate(-1)}
							className="no-drag text-xs opacity-50 hover:opacity-100 transition-opacity"
						>
							← Back
						</button>
					)}
					<span className="text-xs font-mono opacity-50 select-none">
						{title}
					</span>
				</div>
				<div className="no-drag flex items-center gap-4">
					<button
						type="button"
						onClick={handleCloseProject}
						className="text-xs opacity-50 hover:opacity-100 transition-opacity"
					>
						[ close project ]
					</button>
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
	useEffect(() => {
		document.documentElement.classList.add("dark");
	});

	return (
		<HashRouter>
			<Toaster />
			<AuthGate>
				<Routes>
					<Route path="/*" element={<MainApp />} />
					<Route path="/settings" element={<SettingsWindow />} />
				</Routes>
			</AuthGate>
		</HashRouter>
	);
}

export default App;
