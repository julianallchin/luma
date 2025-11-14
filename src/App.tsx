import { invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import type { Graph, NodeTypeDef } from "./bindings/schema";
import "./App.css";
import { createEditor, type EditorController } from "./lib/reteEditor";

type CatalogGroup = {
	category: string;
	nodes: NodeTypeDef[];
};

type RunResult = {
	views: Record<string, number[]>;
};

function groupNodeTypes(nodeTypes: NodeTypeDef[]): CatalogGroup[] {
	const grouped = nodeTypes.reduce<Record<string, NodeTypeDef[]>>(
		(acc, node) => {
			const category = node.category ?? "Unsorted";
			if (!acc[category]) {
				acc[category] = [];
			}
			acc[category].push(node);
			return acc;
		},
		{},
	);

	return Object.entries(grouped)
		.map(([category, nodes]) => ({
			category,
			nodes: nodes.sort((a, b) => a.name.localeCompare(b.name)),
		}))
		.sort((a, b) => a.category.localeCompare(b.category));
}

function App() {
	const [nodeTypes, setNodeTypes] = useState<NodeTypeDef[]>([]);
	const [loadingCatalog, setLoadingCatalog] = useState(false);
	const [catalogError, setCatalogError] = useState<string | null>(null);
	const [graphError, setGraphError] = useState<string | null>(null);
	const [isRunningGraph, setIsRunningGraph] = useState(false);
	const [runResult, setRunResult] = useState<RunResult | null>(null);

	const editorContainerRef = useRef<HTMLDivElement | null>(null);
	const editorRef = useRef<EditorController | null>(null);
	const pendingRunId = useRef(0);
	const nextNodeOffset = useRef({ x: 0, y: 0 });
	const nodeTypesRef = useRef<NodeTypeDef[]>([]);

	useEffect(() => {
		let active = true;
		setLoadingCatalog(true);
		setCatalogError(null);

		invoke<NodeTypeDef[]>("get_node_types")
			.then((types) => {
				if (!active) return;
				setNodeTypes(types);
			})
			.catch((err) => {
				console.error("Failed to fetch node catalog", err);
				if (!active) return;
				setCatalogError(err instanceof Error ? err.message : String(err));
			})
			.finally(() => {
				if (!active) return;
				setLoadingCatalog(false);
			});

		return () => {
			active = false;
		};
	}, []);

	const catalogGroups = useMemo(() => groupNodeTypes(nodeTypes), [nodeTypes]);

	useEffect(() => {
		nodeTypesRef.current = nodeTypes;
	}, [nodeTypes]);

	const serializeGraph = useCallback((): Graph | null => {
		if (!editorRef.current) return null;
		return editorRef.current.serialize();
	}, []);

	const updateViewResults = useCallback(
		async (views: Record<string, number[]>) => {
			if (!editorRef.current) return;
			await editorRef.current.updateViewData(views);
		},
		[],
	);

	const handleGraphChange = useCallback(async () => {
		const graph = serializeGraph();
		if (!graph) return;

		if (graph.nodes.length === 0) {
			setRunResult(null);
			setGraphError(null);
			await updateViewResults({});
			return;
		}

		const runId = ++pendingRunId.current;
		setIsRunningGraph(true);

		try {
			const result = await invoke<RunResult>("run_graph", { graph });
			if (runId !== pendingRunId.current) return;

			setRunResult(result);
			setGraphError(null);
			await updateViewResults(result.views);
		} catch (err) {
			if (runId !== pendingRunId.current) return;
			console.error("Failed to execute graph", err);
			setGraphError(err instanceof Error ? err.message : String(err));
		} finally {
			if (runId === pendingRunId.current) {
				setIsRunningGraph(false);
			}
		}
	}, [serializeGraph, updateViewResults]);

	useEffect(() => {
		if (!editorContainerRef.current) return;
		let destroyed = false;

		(async () => {
			const controller = await createEditor(editorContainerRef.current!, {
				onChange: handleGraphChange,
				getNodeDefinitions: () => nodeTypesRef.current,
			});
			if (destroyed) {
				await controller.destroy();
				return;
			}
			editorRef.current = controller;
		})();

		return () => {
			destroyed = true;
			pendingRunId.current += 1;
			if (editorRef.current) {
				editorRef.current.destroy().catch((error) => {
					console.warn("Failed to destroy editor", error);
				});
				editorRef.current = null;
			}
		};
	}, [handleGraphChange]);

	const handleAddNode = useCallback(async (definition: NodeTypeDef) => {
		if (!editorRef.current) return;
		try {
			const position = { ...nextNodeOffset.current };
			nextNodeOffset.current = {
				x: position.x + 160,
				y: position.y + 120,
			};
			await editorRef.current.addNode(definition, position);
		} catch (err) {
			console.error("Failed to add node", err);
		}
	}, []);

	return (
		<div className="w-screen h-screen bg-background" data-theme="bumblebee">
			<header className="titlebar">
				<div className="no-drag flex items-center gap-1 ml-auto"></div>
			</header>

			<main className="pt-titlebar w-full h-full flex">
				<aside className="w-[320px] border-r border-border bg-muted/50 h-full overflow-y-auto">
					<div className="p-4">
						<h2 className="text-sm font-semibold uppercase tracking-wide text-foreground/70">
							Node Catalog
						</h2>
						{loadingCatalog && (
							<p className="mt-3 text-xs text-foreground/70">Loading nodes…</p>
						)}
						{catalogError && (
							<p className="mt-3 text-xs text-red-500">{catalogError}</p>
						)}
					</div>
					<nav className="space-y-4 px-4 pb-6">
						{catalogGroups.map((group) => (
							<section key={group.category} className="space-y-2">
								<h3 className="text-xs font-medium uppercase tracking-wide text-foreground/60">
									{group.category}
								</h3>
								<ul className="space-y-1">
									{group.nodes.map((node) => (
										<li
											key={node.id}
											className="rounded border border-border/40 bg-background/60 px-3 py-2 shadow-sm cursor-pointer transition-colors hover:border-primary/70"
											onClick={() => handleAddNode(node)}
										>
											<p className="text-sm font-medium text-foreground">
												{node.name}
											</p>
											{node.description && (
												<p className="mt-1 text-xs text-foreground/70">
													{node.description}
												</p>
											)}
										</li>
									))}
								</ul>
							</section>
						))}
					</nav>
				</aside>

				<section className="flex-1 flex flex-col h-full">
					<div
						className="flex-1 border-b border-border bg-background relative"
						ref={editorContainerRef}
					></div>
					<div className="h-56 border-t border-border bg-muted/30 p-4 overflow-y-auto">
						<h2 className="text-sm font-semibold uppercase tracking-wide text-foreground/70">
							Preview
						</h2>
						<div className="mt-2 space-y-3 text-xs text-foreground/70">
							<p>
								Preview waveforms now render directly inside each view_channel
								node.
							</p>
							<p>
								Right-click nodes or connections for actions. Press Delete to
								remove selected nodes.
							</p>
							{isRunningGraph && <p>Running graph…</p>}
							{graphError && <p className="text-red-500">{graphError}</p>}
							{!graphError && !runResult && !isRunningGraph && (
								<p>Build a graph to see live intensity data.</p>
							)}
							{runResult && (
								<p className="text-foreground/60">
									Active view_channels: {Object.keys(runResult.views).length}
								</p>
							)}
						</div>
					</div>
				</section>
			</main>
		</div>
	);
}

export default App;
