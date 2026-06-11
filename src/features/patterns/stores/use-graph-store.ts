import { create } from "zustand";

type NodeParams = Record<string, unknown>;

type GraphStore = {
	nodeParams: Record<string, NodeParams>;
	version: number;
	selectionPreviewSeed: number | null;
	/** Preview-only selection (a tag expression). Patterns always author their
	 * Selection arg as `all`; this overrides what the preview/visualizer renders
	 * on without touching the saved pattern. null → use the args' own value. */
	previewSelection: string | null;
	setParam: (nodeId: string, paramId: string, value: unknown) => void;
	setNodeParams: (nodeId: string, params: NodeParams) => void;
	replaceAll: (entries: Record<string, NodeParams>) => void;
	removeNode: (nodeId: string) => void;
	reset: () => void;
	setSelectionPreviewSeed: (seed: number | null) => void;
	setPreviewSelection: (expression: string | null) => void;
};

export const useGraphStore = create<GraphStore>((set) => ({
	nodeParams: {},
	version: 0,
	selectionPreviewSeed: null,
	previewSelection: null,
	setParam: (nodeId, paramId, value) =>
		set((state) => {
			const existing = state.nodeParams[nodeId] ?? {};
			return {
				nodeParams: {
					...state.nodeParams,
					[nodeId]: { ...existing, [paramId]: value },
				},
				version: state.version + 1,
			};
		}),
	setNodeParams: (nodeId, params) =>
		set((state) => ({
			nodeParams: {
				...state.nodeParams,
				[nodeId]: { ...params },
			},
			version: state.version + 1,
		})),
	replaceAll: (entries) =>
		set((state) => ({
			nodeParams: { ...entries },
			version: state.version + 1,
		})),
	removeNode: (nodeId) =>
		set((state) => {
			if (!(nodeId in state.nodeParams)) {
				return state;
			}
			const next = { ...state.nodeParams };
			delete next[nodeId];
			return { nodeParams: next, version: state.version + 1 };
		}),
	reset: () =>
		set({
			nodeParams: {},
			version: 0,
			selectionPreviewSeed: null,
			previewSelection: null,
		}),
	setSelectionPreviewSeed: (seed) => set({ selectionPreviewSeed: seed }),
	setPreviewSelection: (expression) => set({ previewSelection: expression }),
}));

export function getNodeParamsSnapshot(nodeId: string): NodeParams {
	return useGraphStore.getState().nodeParams[nodeId] ?? {};
}

export function setNodeParamsSnapshot(
	nodeId: string,
	params: NodeParams,
): void {
	useGraphStore.getState().setNodeParams(nodeId, params);
}

export function replaceAllNodeParams(
	entries: Record<string, NodeParams>,
): void {
	useGraphStore.getState().replaceAll(entries);
}

export function removeNodeParams(nodeId: string): void {
	useGraphStore.getState().removeNode(nodeId);
}

export function resetGraphStore(): void {
	useGraphStore.getState().reset();
}
