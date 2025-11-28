import { useDepthBuffer } from "@react-three/drei";
import { useFixtureStore } from "../../universe/stores/use-fixture-store";
import { FixtureObject } from "./fixture-object";

interface FixtureGroupProps {
	enableEditing: boolean;
	transformMode: "translate" | "rotate";
}

export function FixtureGroup({
	enableEditing,
	transformMode,
}: FixtureGroupProps) {
	const patchedFixtures = useFixtureStore((state) => state.patchedFixtures);
	const depthBuffer = useDepthBuffer({ frames: 1 });

	return (
		<group>
			{patchedFixtures.map((fixture) => (
				<FixtureObject
					key={fixture.id}
					fixture={fixture}
					enableEditing={enableEditing}
					transformMode={transformMode}
					depthBuffer={depthBuffer}
				/>
			))}
		</group>
	);
}
