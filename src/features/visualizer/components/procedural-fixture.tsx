import { useFrame } from "@react-three/fiber";
import { useMemo, useRef } from "react";
import { DoubleSide, type Mesh, type MeshStandardMaterial } from "three";
import type {
	FixtureDefinition,
	PatchedFixture,
} from "../../../bindings/fixtures";
import {
	type DmxMapping,
	getDmxMapping,
	getHeadState,
} from "../lib/fixture-utils";
import { dmxStore } from "../stores/dmx-store";

interface ProceduralFixtureProps {
	fixture: PatchedFixture;
	definition: FixtureDefinition;
	modeName: string;
}

export function ProceduralFixture({
	fixture,
	definition,
	modeName,
}: ProceduralFixtureProps) {
	let { Dimensions: dimensions, Layout: layout } = definition.Physical || {};

	// Find active mode
	const activeMode = definition.Mode.find((m) => m["@Name"] === modeName);
	const headCount = activeMode?.Head?.length || 0;

	// Fallback layout logic
	if (
		(!layout || (layout["@Width"] === 1 && layout["@Height"] === 1)) &&
		headCount > 1
	) {
		layout = { "@Width": headCount, "@Height": 1 };
	}

	// Calculate Dimensions (mm -> m)
	const width = (dimensions?.["@Width"] || 200) / 1000;
	const height = (dimensions?.["@Height"] || 200) / 1000;
	const depth = (dimensions?.["@Depth"] || 200) / 1000;

	// Grid logic
	const layoutWidth = layout?.["@Width"] || 1;
	const layoutHeight = layout?.["@Height"] || 1;
	const headWidth = width / layoutWidth;
	const headHeight = height / layoutHeight;

	const headsPositions = useMemo(() => {
		const positions: [number, number, number][] = [];
		const startX = -width / 2 + headWidth / 2;
		const startY = height / 2 - headHeight / 2;

		for (let y = 0; y < layoutHeight; y++) {
			for (let x = 0; x < layoutWidth; x++) {
				const posX = startX + x * headWidth;
				const posY = startY - y * headHeight;
				const posZ = depth / 2 + 0.001;
				positions.push([posX, posY, posZ]);
			}
		}
		return positions;
	}, [width, height, depth, layoutWidth, layoutHeight, headWidth, headHeight]);

	// Pre-calculate DMX Mappings using library
	const headMappings = useMemo(() => {
		const mappings: DmxMapping[] = [];

		// If we have defined heads, map them.
		if (headCount > 0) {
			for (let i = 0; i < headCount; i++) {
				mappings.push(getDmxMapping(definition, modeName, i));
			}
		} else {
			// Single head mode (Head 0)
			mappings.push(getDmxMapping(definition, modeName, 0));
		}
		return mappings;
	}, [definition, modeName, headCount]);

	// Refs for mesh updates
	const meshRefs = useRef<(Mesh | null)[]>([]);

	// Animation Loop
	useFrame((ctx) => {
		const universeData = dmxStore.getUniverse(Number(fixture.universe));
		if (!universeData) return;

		const startAddress = Number(fixture.address) - 1; // 0-based
		const pixelsPerHead = headsPositions.length / headMappings.length;
		const time = ctx.clock.getElapsedTime();

		headsPositions.forEach((_, i) => {
			const mesh = meshRefs.current[i];

			// Distribute pixels to heads
			let mappingIndex = 0;
			if (headMappings.length > 0) {
				mappingIndex = Math.floor(i / pixelsPerHead);
				if (mappingIndex >= headMappings.length)
					mappingIndex = headMappings.length - 1;
			}

			const mapping = headMappings[mappingIndex];

			if (mesh && mapping) {
				const state = getHeadState(mapping, universeData, startAddress);

				// Strobe Logic
				let intensity = state.intensity;
				if (state.strobe > 9) {
					// Map DMX 10-255 to 1-30 Hz
					const hz = 1 + ((state.strobe - 10) / 245) * 29;
					const period = 1 / hz;
					// 50% duty cycle square wave
					const isOff = time % period > period * 0.5;
					if (isOff) {
						intensity = 0;
					}
				}

				// Update material
				const mat = mesh.material as MeshStandardMaterial;
				mat.emissive.setRGB(state.color.r, state.color.g, state.color.b);
				mat.emissiveIntensity = intensity;
			}
		});
	});

	return (
		<group>
			{/* Main Casing Body */}
			<mesh>
				<boxGeometry args={[width, height, depth]} />
				<meshStandardMaterial color="#222" />
			</mesh>

			{/* Heads / Pixels */}

			{headsPositions.map((pos, i) => (
				<mesh
					// biome-ignore lint/suspicious/noArrayIndexKey: Array index is used as key for static generated geometry and won't change order.
					key={i}
					position={[pos[0], pos[1], pos[2]]}
					ref={(el) => {
						meshRefs.current[i] = el;
					}}
				>
					<planeGeometry args={[headWidth * 0.9, headHeight * 0.9]} />

					<meshStandardMaterial
						color="#000000"
						emissive="#000000"
						emissiveIntensity={1}
						side={DoubleSide}
						toneMapped={false} // Helps with bloom
					/>
				</mesh>
			))}
		</group>
	);
}
