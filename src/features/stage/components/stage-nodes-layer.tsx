import { Suspense, useCallback } from "react";
import type { Group } from "three";
import type { ResolvedNode } from "@/bindings/venue-graph";
import { registerNodeGroup, unregisterNodeGroup } from "../lib/node-refs";
import { catalogMeshUrl } from "../lib/stage-meshes";
import { useStructureNodes } from "../stores/use-venue-store";
import { StageNodeMesh } from "./stage-node-mesh";

/**
 * Draws the solved venue's structure.
 *
 * Each node is placed at its **world** pose — the resolver already collapsed
 * the parent chain, so there is no group nesting here. A node with no GLB is
 * skipped: the truss family is procedural and this app has no generator, so
 * generated truss is drawn by the gpui builder.
 */
export function StageNodesLayer() {
	const nodes = useStructureNodes();

	return (
		<Suspense fallback={null}>
			{nodes.map((node) => {
				const url = catalogMeshUrl(node.catalogRef ?? "");
				if (!url) return null;
				return <StageNode key={node.id} node={node} url={url} />;
			})}
		</Suspense>
	);
}

function StageNode({ node, url }: { node: ResolvedNode; url: string }) {
	const setGroupRef = useCallback(
		(group: Group | null) => {
			if (group) registerNodeGroup(node.id, group);
			else unregisterNodeGroup(node.id);
		},
		[node.id],
	);

	return (
		<group
			ref={setGroupRef}
			// Data space is Z-up, three.js is Y-up: swap Y↔Z.
			position={[node.position[0], node.position[2], node.position[1]]}
			rotation={[node.rotation[0], node.rotation[2], node.rotation[1]]}
		>
			<StageNodeMesh url={url} />
		</group>
	);
}
