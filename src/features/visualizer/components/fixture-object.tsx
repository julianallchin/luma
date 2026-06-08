import {
	Suspense,
	useCallback,
	useEffect,
	useLayoutEffect,
	useMemo,
	useRef,
	useState,
} from "react";
import type { Group } from "three";
import { useStagePieceStore } from "@/features/stage/stores/use-stage-piece-store";
import type {
	FixtureDefinition,
	PatchedFixture,
} from "../../../bindings/fixtures";
import { useFixtureStore } from "../../universe/stores/use-fixture-store";
import {
	registerFixtureGroup,
	unregisterFixtureGroup,
} from "../lib/fixture-refs";
import { getModelForFixture, isProcedural } from "./fixture-models";
import { ProceduralFixture } from "./procedural-fixture";
import { StaticFixture } from "./static-fixture";

interface FixtureObjectProps {
	fixture: PatchedFixture;
	enableEditing: boolean;
	hideBeams?: boolean;
}

export function FixtureObject({
	fixture,
	enableEditing: _enableEditing,
	hideBeams = false,
}: FixtureObjectProps) {
	const groupRef = useRef<Group>(null);

	const getDefinition = useFixtureStore((state) => state.getDefinition);
	const selectFixtureById = useFixtureStore((state) => state.selectFixtureById);
	const previewFixtureIds = useFixtureStore((state) => state.previewFixtureIds);

	// Subscribe to multi-selection state with selectors to avoid full-set re-renders
	const isSelected = useFixtureStore((state) =>
		state.selectedPatchedIds.has(fixture.id),
	);
	const isPrimary = useFixtureStore(
		(state) => state.lastSelectedPatchedId === fixture.id,
	);
	const isPreviewed = !isSelected && previewFixtureIds.includes(fixture.id);

	// Register the group in the module-level ref map so the unified
	// gizmo (rendered at the layer level) can read its live world pose
	// without prop-drilling.
	const setGroupRef = useCallback(
		(node: Group | null) => {
			groupRef.current = node;
			if (node) {
				registerFixtureGroup(fixture.id, node);
			} else {
				unregisterFixtureGroup(fixture.id);
			}
		},
		[fixture.id],
	);

	useEffect(() => {
		return () => unregisterFixtureGroup(fixture.id);
	}, [fixture.id]);

	// Set position/rotation imperatively so multi-selection drag overrides aren't
	// clobbered by React re-renders (declarative position would reset on render)
	useLayoutEffect(() => {
		if (groupRef.current) {
			// Z-up (data) to Y-up (Three.js): swap Y↔Z
			groupRef.current.position.set(fixture.posX, fixture.posZ, fixture.posY);
			groupRef.current.rotation.set(fixture.rotX, fixture.rotZ, fixture.rotY);
		}
	}, [
		fixture.posX,
		fixture.posY,
		fixture.posZ,
		fixture.rotX,
		fixture.rotY,
		fixture.rotZ,
	]);

	const [definition, setDefinition] = useState<FixtureDefinition | null>(null);

	useEffect(() => {
		getDefinition(fixture.fixturePath).then(setDefinition);
	}, [fixture.fixturePath, getDefinition]);

	// Determine content based on definition type
	const fallbackVisual = (
		<mesh>
			<boxGeometry args={[0.2, 0.2, 0.2]} />
			<meshStandardMaterial color="#555" />
		</mesh>
	);

	let visual = fallbackVisual;

	if (definition) {
		const procedural = isProcedural(definition);
		const modelInfo = getModelForFixture(definition);

		if (!procedural && modelInfo) {
			visual = (
				<Suspense fallback={fallbackVisual}>
					<StaticFixture
						fixture={fixture}
						definition={definition}
						model={modelInfo}
						hideBeams={hideBeams}
					/>
				</Suspense>
			);
		} else {
			visual = (
				<ProceduralFixture
					fixture={fixture}
					definition={definition}
					modeName={fixture.modeName}
				/>
			);
		}
	}

	// Calculate expected dimensions from fixture definition
	const { width, height, depth } = useMemo(() => {
		const dim = definition?.Physical?.Dimensions;
		return {
			width: (dim?.["@Width"] ?? 0) / 1000,
			height: (dim?.["@Height"] ?? 0) / 1000,
			depth: (dim?.["@Depth"] ?? 0) / 1000,
		};
	}, [definition]);

	const content = (
		// biome-ignore lint/a11y/noStaticElementInteractions: 3D object interaction
		<group
			ref={setGroupRef}
			onClick={(e) => {
				e.stopPropagation();
				const shift = (e.nativeEvent as PointerEvent).shiftKey;
				selectFixtureById(fixture.id, { shift });
				// Cross-type clear: a non-shift click on a fixture also
				// drops any stage-piece selection (and vice versa in
				// stage-piece-node.tsx). Shift-click preserves both.
				if (!shift) useStagePieceStore.getState().clearSelection();
			}}
		>
			{visual}
			{isPrimary && (
				<mesh>
					<boxGeometry args={[width, height, depth]} />
					<meshBasicMaterial color="yellow" wireframe />
				</mesh>
			)}
			{isSelected && !isPrimary && (
				<mesh>
					<boxGeometry args={[width, height, depth]} />
					<meshBasicMaterial color="#b8b846" wireframe />
				</mesh>
			)}
			{isPreviewed && (
				<mesh>
					<boxGeometry args={[width * 1.05, height * 1.05, depth * 1.05]} />
					<meshBasicMaterial color="#38bdf8" wireframe />
				</mesh>
			)}
		</group>
	);

	return content;
}
