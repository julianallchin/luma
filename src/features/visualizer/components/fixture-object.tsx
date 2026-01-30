import { TransformControls } from "@react-three/drei";
import { Suspense, useEffect, useMemo, useRef, useState } from "react";
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
}

export function FixtureObject({
	fixture,
	enableEditing,
	transformMode,
}: FixtureObjectProps) {
	const groupRef = useRef<Group>(null);
	const moveFixtureSpatial = useFixtureStore(
		(state) => state.moveFixtureSpatial,
	);
	const getDefinition = useFixtureStore((state) => state.getDefinition);
	const selectedPatchedId = useFixtureStore((state) => state.selectedPatchedId);
	const previewFixtureIds = useFixtureStore((state) => state.previewFixtureIds);
	const setSelectedPatchedId = useFixtureStore(
		(state) => state.setSelectedPatchedId,
	);

	const [definition, setDefinition] = useState<FixtureDefinition | null>(null);
	const isSelected = selectedPatchedId === fixture.id;
	const isPreviewed = !isSelected && previewFixtureIds.includes(fixture.id);

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
			ref={groupRef}
			position={[fixture.posX, fixture.posY, fixture.posZ]}
			rotation={[fixture.rotX, fixture.rotY, fixture.rotZ]}
			onClick={(e) => {
				e.stopPropagation();
				setSelectedPatchedId(fixture.id);
			}}
		>
			{visual}
			{isSelected && (
				<mesh>
					<boxGeometry args={[width, height, depth]} />
					<meshBasicMaterial color="yellow" wireframe />
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

	if (enableEditing && isSelected) {
		return (
			<TransformControls
				object={groupRef as React.RefObject<Group>}
				mode={transformMode}
				rotationSnap={
					transformMode === "rotate" ? MathUtils.degToRad(15) : undefined
				}
				onMouseUp={() => {
					if (groupRef.current) {
						const { position, rotation } = groupRef.current;
						moveFixtureSpatial(
							fixture.id,
							{
								x: position.x,
								y: position.y,
								z: position.z,
							},
							{
								x: rotation.x,
								y: rotation.y,
								z: rotation.z,
							},
						);
					}
				}}
			>
				{content}
			</TransformControls>
		);
	}

	return content;
}
