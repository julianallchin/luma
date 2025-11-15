import { create } from "zustand";

type NodeParams = Record<string, unknown>;

type GraphStore = {
	nodeParams: Record<string, NodeParams>;
	version: number;
	setParam: (nodeId: string, paramId: string, value: unknown) => void;
	setNodeParams: (nodeId: string, params: NodeParams) => void;
	replaceAll: (entries: Record<string, NodeParams>) => void;
	removeNode: (nodeId: string) => void;
	reset: () => void;
};

export const useGraphStore = create<GraphStore>((set) => ({
	nodeParams: {},
	version: 0,
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
	reset: () => set({ nodeParams: {}, version: 0 }),
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
