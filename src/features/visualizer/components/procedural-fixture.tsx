import { useFrame } from "@react-three/fiber";
import { useContext, useMemo, useRef } from "react";
import {
	DoubleSide,
	Euler,
	type Mesh,
	type MeshStandardMaterial,
	Quaternion,
	Vector3,
} from "three";
import type {
	FixtureDefinition,
	PatchedFixture,
} from "../../../bindings/fixtures";
import { PrimitiveOverrideContext } from "../hooks/use-primitive-state";
import { submitLight } from "../lib/spotlight-pool";
import { universeStore } from "../stores/universe-state-store";

interface ProceduralFixtureProps {
	fixture: PatchedFixture;
	definition: FixtureDefinition;
	modeName: string;
}

const _localPos = new Vector3();
const _faceDir = new Vector3();
const _qFixture = new Quaternion();
const _euler = new Euler();

export function ProceduralFixture({
	fixture,
	definition,
	modeName,
}: ProceduralFixtureProps) {
	const overrideGetter = useContext(PrimitiveOverrideContext);
	let { Dimensions: dimensions, Layout: layout } = definition.Physical || {};

	const activeMode = definition.Mode.find((m) => m["@Name"] === modeName);
	const headCount = activeMode?.Head?.length || 0;

	if (
		(!layout || (layout["@Width"] === 1 && layout["@Height"] === 1)) &&
		headCount > 1
	) {
		layout = { "@Width": headCount, "@Height": 1 };
	}

	const width = (dimensions?.["@Width"] || 200) / 1000;
	const height = (dimensions?.["@Height"] || 200) / 1000;
	const depth = (dimensions?.["@Depth"] || 200) / 1000;

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

	const meshRefs = useRef<(Mesh | null)[]>([]);

	useFrame((ctx) => {
		const pixelsPerHead = headsPositions.length / Math.max(1, headCount);
		const time = ctx.clock.getElapsedTime();

		// Fixture world rotation
		_euler.set(fixture.rotX, fixture.rotZ, fixture.rotY);
		_qFixture.setFromEuler(_euler);

		// Face direction in world space
		_faceDir.set(0, 0, 1).applyQuaternion(_qFixture).normalize();

		const fxX = fixture.posX;
		const fxY = fixture.posZ;
		const fxZ = fixture.posY;

		// Update pixel emissives + submit light requests for brightest heads
		let brightestIntensity = 0;
		let brightestColor: [number, number, number] = [0, 0, 0];

		headsPositions.forEach((_, i) => {
			const mesh = meshRefs.current[i];
			if (!mesh) return;

			let headIndex = 0;
			if (headCount > 0) {
				headIndex = Math.floor(i / pixelsPerHead);
				if (headIndex >= headCount) headIndex = headCount - 1;
			}

			const primitiveId = `${fixture.id}:${headIndex}`;
			const state = overrideGetter
				? overrideGetter()(primitiveId)
				: universeStore.getPrimitive(primitiveId);
			let intensity = state?.dimmer ?? 0;
			const color = state?.color ?? [0, 0, 0];

			if (state && state.strobe > 0) {
				const hz = state.strobe * 10;
				if (hz > 0) {
					const period = 1 / hz;
					if (time % period > period * 0.5) intensity = 0;
				}
			}

			const mat = mesh.material as MeshStandardMaterial;
			mat.emissive.setRGB(color[0], color[1], color[2]);
			mat.emissiveIntensity = intensity * 5;

			if (intensity > brightestIntensity) {
				brightestIntensity = intensity;
				brightestColor = color;
			}
		});

		// Submit a single aggregate light for the whole fixture (skip in preview)
		if (brightestIntensity > 0.01 && !overrideGetter) {
			_localPos.set(0, 0, depth / 2 + 0.05).applyQuaternion(_qFixture);
			submitLight({
				posX: fxX + _localPos.x,
				posY: fxY + _localPos.y,
				posZ: fxZ + _localPos.z,
				dirX: _faceDir.x,
				dirY: _faceDir.y,
				dirZ: _faceDir.z,
				r: brightestColor[0],
				g: brightestColor[1],
				b: brightestColor[2],
				intensity: brightestIntensity * 8,
				angle: Math.PI / 3,
				distance: 4,
			});
		}
	});

	return (
		<group>
			<mesh castShadow receiveShadow>
				<boxGeometry args={[width, height, depth]} />
				<meshStandardMaterial color="#050505" />
			</mesh>

			{headsPositions.map((pos, i) => (
				<mesh
					// biome-ignore lint/suspicious/noArrayIndexKey: static geometry
					key={i}
					position={[pos[0], pos[1], pos[2]]}
					ref={(el) => {
						meshRefs.current[i] = el;
					}}
					castShadow
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
