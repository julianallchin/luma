import { TransformControls } from "@react-three/drei";
import type React from "react";
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
import { MathUtils } from "three";
import type {
	FixtureDefinition,
	PatchedFixture,
} from "../../../bindings/fixtures";
import { useFixtureStore } from "../../universe/stores/use-fixture-store";
import { getModelForFixture, isProcedural } from "./fixture-models";
import { ProceduralFixture } from "./procedural-fixture";
import { StaticFixture } from "./static-fixture";

interface FixtureObjectProps {
	fixture: PatchedFixture;
	enableEditing: boolean;
	transformMode: "translate" | "rotate";
	onGroupRef?: (id: string, ref: Group | null) => void;
}

export function FixtureObject({
	fixture,
	enableEditing,
	transformMode,
	onGroupRef,
}: FixtureObjectProps) {
	const groupRef = useRef<Group>(null);

	const moveFixtureSpatial = useFixtureStore(
		(state) => state.moveFixtureSpatial,
	);
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
	const selectionSize = useFixtureStore(
		(state) => state.selectedPatchedIds.size,
	);
	const isPreviewed = !isSelected && previewFixtureIds.includes(fixture.id);

	// Register group ref with parent for multi-selection transforms
	const setGroupRef = useCallback(
		(node: Group | null) => {
			groupRef.current = node;
			onGroupRef?.(fixture.id, node);
		},
		[fixture.id, onGroupRef],
	);

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
				selectFixtureById(fixture.id, {
					shift: (e.nativeEvent as PointerEvent).shiftKey,
				});
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

	return (
		<>
			{enableEditing && isPrimary && selectionSize === 1 && (
				<TransformControls
					object={groupRef as React.RefObject<Group>}
					mode={transformMode}
					rotationSnap={
						transformMode === "rotate" ? MathUtils.degToRad(15) : undefined
					}
					onMouseUp={() => {
						if (groupRef.current) {
							const { position, rotation } = groupRef.current;
							// Y-up (Three.js) to Z-up (data): swap Y↔Z
							moveFixtureSpatial(
								fixture.id,
								{ x: position.x, y: position.z, z: position.y },
								{ x: rotation.x, y: rotation.z, z: rotation.y },
							);
						}
					}}
				/>
			)}
			{content}
		</>
	);
}
