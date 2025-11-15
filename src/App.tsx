import { invoke } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";

import type { NodeTypeDef } from "./bindings/schema";
import "./App.css";
import { PatternEditor } from "./components/patterns/PatternEditor";
import { PatternList } from "./components/patterns/PatternList";
import { TrackList } from "./components/tracks/TrackList";
import { useAppViewStore } from "./useAppViewStore";

function App() {
	const view = useAppViewStore((state) => state.view);
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
		document.documentElement.setAttribute("data-theme", "business");
	}, []);

	return (
		<div className="w-screen h-screen bg-background">
			<header className="titlebar">
				<div className="no-drag flex items-center gap-1 ml-auto"></div>
			</header>

			<main className="pt-titlebar w-full h-full">
				{view.type === "welcome" ? (
					<div className="h-full w-full p-4">
						<div className="grid h-full min-h-0 gap-4 lg:grid-cols-[1.3fr,1fr]">
							<PatternList />
							<TrackList />
						</div>
					</div>
				) : (
					<PatternEditor
						patternId={view.patternId}
						patternName={view.name}
						nodeTypes={nodeTypes}
					/>
				)}
			</main>
		</div>
	);
}

export default App;
