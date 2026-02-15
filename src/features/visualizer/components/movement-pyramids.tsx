import { Line } from "@react-three/drei";
import { useMemo } from "react";
import * as THREE from "three";
import type { PatchedFixture } from "@/bindings/fixtures";
import type { MovementConfig } from "@/bindings/groups";
import { useFixtureStore } from "../../universe/stores/use-fixture-store";
import { useGroupStore } from "../../universe/stores/use-group-store";

const PYRAMID_LENGTH = 1.5; // meters
const PYRAMID_COLOR = "#facc15"; // amber/yellow
const PYRAMID_SEGMENTS = 24; // ellipse resolution

/**
 * Compute the UV axes and 4 corner rays of the movement pyramid.
 * Returns points in Three.js Y-up coordinate system.
 */
function computePyramidPoints(
	fixture: PatchedFixture,
	config: MovementConfig,
): { origin: THREE.Vector3; corners: THREE.Vector3[]; rim: THREE.Vector3[] } {
	// Fixture position: data Z-up → Three.js Y-up
	const origin = new THREE.Vector3(fixture.posX, fixture.posZ, fixture.posY);

	// Base direction: data Z-up → Three.js Y-up
	const baseDir = new THREE.Vector3(
		config.baseDirX,
		config.baseDirZ,
		config.baseDirY,
	).normalize();

	// Derive UV axes via cross product with world up
	const worldUp = new THREE.Vector3(0, 1, 0);
	let axisU = new THREE.Vector3().crossVectors(baseDir, worldUp);
	if (axisU.lengthSq() < 1e-6) {
		// baseDir is parallel to world up, use world forward
		axisU.crossVectors(baseDir, new THREE.Vector3(0, 0, 1));
	}
	axisU.normalize();
	const axisV = new THREE.Vector3().crossVectors(baseDir, axisU).normalize();

	// Apply UV rotation
	if (config.uvRotation !== 0) {
		const rotRad = THREE.MathUtils.degToRad(config.uvRotation);
		const cos = Math.cos(rotRad);
		const sin = Math.sin(rotRad);
		const uNew = axisU
			.clone()
			.multiplyScalar(cos)
			.add(axisV.clone().multiplyScalar(sin));
		const vNew = axisU
			.clone()
			.multiplyScalar(-sin)
			.add(axisV.clone().multiplyScalar(cos));
		axisU = uNew.normalize();
		axisV.copy(vNew.normalize());
	}

	const extentURad = THREE.MathUtils.degToRad(config.extentU);
	const extentVRad = THREE.MathUtils.degToRad(config.extentV);

	// Generate elliptical rim points
	const rim: THREE.Vector3[] = [];
	for (let i = 0; i < PYRAMID_SEGMENTS; i++) {
		const theta = (i / PYRAMID_SEGMENTS) * Math.PI * 2;
		const u = Math.cos(theta);
		const v = Math.sin(theta);
		const angleU = u * extentURad;
		const angleV = v * extentVRad;

		// Rotate baseDir by angular offsets around UV axes (small-angle approx is fine for visualization)
		const dir = baseDir
			.clone()
			.add(axisU.clone().multiplyScalar(Math.tan(angleU)))
			.add(axisV.clone().multiplyScalar(Math.tan(angleV)))
			.normalize();

		rim.push(origin.clone().add(dir.multiplyScalar(PYRAMID_LENGTH)));
	}

	// 4 corner points (at ±extentU, ±extentV)
	const corners: THREE.Vector3[] = [];
	for (const [su, sv] of [
		[1, 0],
		[0, 1],
		[-1, 0],
		[0, -1],
	] as const) {
		const angleU = su * extentURad;
		const angleV = sv * extentVRad;
		const dir = baseDir
			.clone()
			.add(axisU.clone().multiplyScalar(Math.tan(angleU)))
			.add(axisV.clone().multiplyScalar(Math.tan(angleV)))
			.normalize();
		corners.push(origin.clone().add(dir.multiplyScalar(PYRAMID_LENGTH)));
	}

	return { origin, corners, rim };
}

function FixturePyramid({
	fixture,
	config,
}: {
	fixture: PatchedFixture;
	config: MovementConfig;
}) {
	const { origin, corners, rim } = useMemo(
		() => computePyramidPoints(fixture, config),
		[fixture, config],
	);

	const originArr = origin.toArray() as [number, number, number];
	const rimArr = useMemo(
		() => [
			...rim.map((p) => p.toArray() as [number, number, number]),
			rim[0].toArray() as [number, number, number], // close the loop
		],
		[rim],
	);

	return (
		<group>
			{/* 4 edge lines from origin to cardinal rim points */}
			{corners.map((corner, i) => (
				<Line
					// biome-ignore lint/suspicious/noArrayIndexKey: static geometry
					key={i}
					points={[originArr, corner.toArray() as [number, number, number]]}
					color={PYRAMID_COLOR}
					lineWidth={1}
					opacity={0.5}
					transparent
				/>
			))}
			{/* Elliptical rim */}
			<Line
				points={rimArr}
				color={PYRAMID_COLOR}
				lineWidth={1}
				opacity={0.4}
				transparent
			/>
		</group>
	);
}

export function MovementPyramids() {
	const selectedGroupId = useGroupStore((s) => s.selectedGroupId);
	const groups = useGroupStore((s) => s.groups);
	const patchedFixtures = useFixtureStore((s) => s.patchedFixtures);

	const selectedGroup = groups.find((g) => g.groupId === selectedGroupId);
	const config = selectedGroup?.movementConfig;

	const fixtureMap = useMemo(
		() => new Map(patchedFixtures.map((f) => [f.id, f])),
		[patchedFixtures],
	);

	if (!selectedGroup || !config) return null;

	// Only show for mover types
	if (
		selectedGroup.fixtureType !== "moving_head" &&
		selectedGroup.fixtureType !== "scanner"
	) {
		return null;
	}

	return (
		<group>
			{selectedGroup.fixtures.map((gf) => {
				const fixture = fixtureMap.get(gf.id);
				if (!fixture) return null;
				return <FixturePyramid key={gf.id} fixture={fixture} config={config} />;
			})}
		</group>
	);
}
