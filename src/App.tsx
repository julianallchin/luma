import { invoke } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";

import type { NodeTypeDef } from "./bindings/schema";
import "./App.css";
import { ProjectDashboard } from "./features/app/components/project-dashboard";
import { WelcomeScreen } from "./features/app/components/welcome-screen";
import { useAppViewStore } from "./features/app/stores/use-app-view-store";
import { PatternEditor } from "./features/patterns/components/pattern-editor";
import { TrackEditor } from "./features/track-editor/components/track-editor";
import { UniverseDesigner } from "./features/universe/components/UniverseDesigner";

function App() {
	const view = useAppViewStore((state) => state.view);
	const currentProject = useAppViewStore((state) => state.currentProject);
	const setProject = useAppViewStore((state) => state.setProject);
	const goBack = useAppViewStore((state) => state.goBack);
	const [nodeTypes, setNodeTypes] = useState<NodeTypeDef[]>([]);

	// Load node types only when needed (in pattern editor)
	useEffect(() => {
		if (view.type !== "pattern") return;

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
	}, [view.type]);

	useEffect(() => {
		// Enable dark mode
		document.documentElement.classList.add("dark");
	});

	const handleCloseProject = async () => {
		try {
			await invoke("close_project");
			setProject(null);
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

	return (
		<div className="w-screen h-screen bg-background">
			<header
				className="titlebar flex justify-between items-center pr-4"
				data-tauri-drag-region
			>
				<div className="pl-16 flex items-center gap-3">
					{view.type !== "welcome" && (
						<button
							type="button"
							onClick={goBack}
							className="no-drag text-xs opacity-50 hover:opacity-100 transition-opacity"
						>
							← Back
						</button>
					)}
					<span className="text-xs font-mono opacity-50 select-none">
						{view.type === "trackEditor"
							? view.trackName
							: view.type === "pattern"
								? view.name
								: view.type === "universe"
									? "Universe Designer"
									: currentProject.name}
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
				</div>
			</header>

			<main className="pt-titlebar w-full h-full">
				{view.type === "welcome" ? (
					<ProjectDashboard />
				) : view.type === "pattern" ? (
					<PatternEditor patternId={view.patternId} nodeTypes={nodeTypes} />
				) : view.type === "trackEditor" ? (
					<TrackEditor trackId={view.trackId} trackName={view.trackName} />
				) : view.type === "universe" ? (
					<UniverseDesigner />
				) : null}
			</main>
		</div>
	);
}

export default App;
