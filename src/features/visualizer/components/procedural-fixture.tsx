import { useFrame } from "@react-three/fiber";
import { useMemo, useRef } from "react";
import { DoubleSide, type Mesh, type MeshStandardMaterial } from "three";
import type {
	FixtureDefinition,
	PatchedFixture,
} from "../../../bindings/fixtures";
import { universeStore } from "../stores/universe-state-store";

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

	// Refs for mesh updates
	const meshRefs = useRef<(Mesh | null)[]>([]);

	// Animation Loop
	useFrame((ctx) => {
		const pixelsPerHead = headsPositions.length / Math.max(1, headCount);
		const time = ctx.clock.getElapsedTime();

		headsPositions.forEach((_, i) => {
			const mesh = meshRefs.current[i];
			if (!mesh) return;

			// Determine which head index this pixel belongs to
			let headIndex = 0;
			if (headCount > 0) {
				headIndex = Math.floor(i / pixelsPerHead);
				if (headIndex >= headCount) headIndex = headCount - 1;
			}

			// Lookup state by ID
			const primitiveId = `${fixture.id}:${headIndex}`;
			const state = universeStore.getPrimitive(primitiveId);

			// Default state if not found
			let intensity = state?.dimmer ?? 0;
			const color = state?.color ?? [0, 0, 0];
			const strobe = state?.strobe ?? 0;

			// Strobe Logic
			if (strobe > 0) {
				const hz = strobe * 10;
				if (hz > 0) {
					const period = 1 / hz;
					const isOff = time % period > period * 0.5;
					if (isOff) {
						intensity = 0;
					}
				}
			}

			// Update material
			const mat = mesh.material as MeshStandardMaterial;
			mat.emissive.setRGB(color[0], color[1], color[2]);
			mat.emissiveIntensity = intensity * 5;
		});
	});

	return (
		<group>
			{/* Main Casing Body */}
			<mesh>
				<boxGeometry args={[width, height, depth]} />
				<meshStandardMaterial color="#050505" />
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
						toneMapped={false}
					/>
				</mesh>
			))}
		</group>
	);
}
