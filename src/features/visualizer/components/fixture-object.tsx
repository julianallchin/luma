import { TransformControls } from "@react-three/drei";
import { useEffect, useRef, useState } from "react";
import type { DepthTexture, Group } from "three";
import { MathUtils } from "three";
import type {
	FixtureDefinition,
	PatchedFixture,
} from "../../../bindings/fixtures";
import { useFixtureStore } from "../../universe/stores/use-fixture-store";
import { ProceduralFixture } from "./procedural-fixture";
import { getModelForFixture, isProcedural } from "./fixture-models";
import { StaticFixture } from "./static-fixture";

interface FixtureObjectProps {
	fixture: PatchedFixture;
	enableEditing: boolean;
	transformMode: "translate" | "rotate";
	depthBuffer: DepthTexture;
}

export function FixtureObject({
	fixture,
	enableEditing,
	transformMode,
	depthBuffer,
}: FixtureObjectProps) {
	const groupRef = useRef<Group>(null);
	const moveFixtureSpatial = useFixtureStore(
		(state) => state.moveFixtureSpatial,
	);
	const getDefinition = useFixtureStore((state) => state.getDefinition);
	const selectedPatchedId = useFixtureStore((state) => state.selectedPatchedId);
	const setSelectedPatchedId = useFixtureStore(
		(state) => state.setSelectedPatchedId,
	);

	const [definition, setDefinition] = useState<FixtureDefinition | null>(null);
	const isSelected = selectedPatchedId === fixture.id;

	useEffect(() => {
		getDefinition(fixture.fixturePath).then(setDefinition);
	}, [fixture.fixturePath, getDefinition]);

	// Determine content based on definition type
	let visual = (
		<mesh>
			<boxGeometry args={[0.2, 0.2, 0.2]} />
			<meshStandardMaterial color="#555" />
		</mesh>
	);

	if (definition) {
		const procedural = isProcedural(definition);
		const modelInfo = getModelForFixture(definition);

		if (!procedural && modelInfo) {
			visual = (
				<StaticFixture
					fixture={fixture}
					definition={definition}
					modeName={fixture.modeName}
					model={modelInfo}
					depthBuffer={depthBuffer}
				/>
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
					<boxGeometry args={[0.25, 0.25, 0.25]} />
					<meshBasicMaterial color="yellow" wireframe />
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
