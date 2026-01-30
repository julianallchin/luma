import { Line, Text } from "@react-three/drei";
import { useEffect, useMemo } from "react";
import * as THREE from "three";
import type { FixtureDefinition, PatchedFixture } from "@/bindings/fixtures";
import { useFixtureStore } from "../../universe/stores/use-fixture-store";
import { useGroupStore } from "../../universe/stores/use-group-store";
import { FixtureObject } from "./fixture-object";

interface FixtureGroupProps {
	enableEditing: boolean;
	transformMode: "translate" | "rotate";
}

interface BoundingBox {
	groupId: number;
	groupName: string | null;
	tags: string[];
	min: [number, number, number];
	max: [number, number, number];
	color: string;
}

// Nice pastel colors for groups
const GROUP_COLORS = [
	"#7eb8da", // soft blue
	"#a8d8a8", // soft green
	"#f4a6a6", // soft red/pink
	"#c9a8f4", // soft purple
	"#f4d8a8", // soft orange
	"#a8f4f4", // soft cyan
	"#f4a8d8", // soft magenta
	"#d8f4a8", // soft lime
	"#a8c8f4", // soft periwinkle
	"#f4c8a8", // soft peach
];

function getRotatedBounds(
	fixture: PatchedFixture,
	definition: FixtureDefinition | undefined,
): { min: THREE.Vector3; max: THREE.Vector3 } {
	// Get actual dimensions from definition (mm -> m)
	const dim = definition?.Physical?.Dimensions;
	const width = (dim?.["@Width"] ?? 0) / 1000;
	const height = (dim?.["@Height"] ?? 0) / 1000;
	const depth = (dim?.["@Depth"] ?? 0) / 1000;

	const halfW = width / 2;
	const halfH = height / 2;
	const halfD = depth / 2;

	// Create a box representing the fixture with actual dimensions
	const corners = [
		new THREE.Vector3(-halfW, -halfH, -halfD),
		new THREE.Vector3(halfW, -halfH, -halfD),
		new THREE.Vector3(-halfW, halfH, -halfD),
		new THREE.Vector3(halfW, halfH, -halfD),
		new THREE.Vector3(-halfW, -halfH, halfD),
		new THREE.Vector3(halfW, -halfH, halfD),
		new THREE.Vector3(-halfW, halfH, halfD),
		new THREE.Vector3(halfW, halfH, halfD),
	];

	// Apply rotation (values are already in radians)
	const euler = new THREE.Euler(fixture.rotX, fixture.rotY, fixture.rotZ);
	const quaternion = new THREE.Quaternion().setFromEuler(euler);

	// Transform corners and find bounds
	const min = new THREE.Vector3(Infinity, Infinity, Infinity);
	const max = new THREE.Vector3(-Infinity, -Infinity, -Infinity);

	for (const corner of corners) {
		corner.applyQuaternion(quaternion);
		corner.add(new THREE.Vector3(fixture.posX, fixture.posY, fixture.posZ));
		min.min(corner);
		max.max(corner);
	}

	return { min, max };
}

function GroupBoundingBox({ box }: { box: BoundingBox }) {
	const { min, max, color, groupName } = box;

	// Create wireframe box vertices
	const points: [number, number, number][] = [
		// Bottom face
		[min[0], min[1], min[2]],
		[max[0], min[1], min[2]],
		[max[0], min[1], max[2]],
		[min[0], min[1], max[2]],
		[min[0], min[1], min[2]],
		// Connect to top
		[min[0], max[1], min[2]],
		// Top face
		[max[0], max[1], min[2]],
		[max[0], max[1], max[2]],
		[min[0], max[1], max[2]],
		[min[0], max[1], min[2]],
	];

	// Vertical edges
	const verticals: [number, number, number][][] = [
		[
			[max[0], min[1], min[2]],
			[max[0], max[1], min[2]],
		],
		[
			[max[0], min[1], max[2]],
			[max[0], max[1], max[2]],
		],
		[
			[min[0], min[1], max[2]],
			[min[0], max[1], max[2]],
		],
	];

	// Label position - top corner of box
	const labelPos: [number, number, number] = [min[0], max[1] + 0.1, min[2]];

	return (
		<group>
			<Line
				points={points}
				color={color}
				lineWidth={1.5}
				opacity={0.6}
				transparent
			/>
			{verticals.map((edge, i) => (
				<Line
					// biome-ignore lint/suspicious/noArrayIndexKey: static geometry edges
					key={i}
					points={edge}
					color={color}
					lineWidth={1.5}
					opacity={0.6}
					transparent
				/>
			))}
			{groupName && (
				<Text
					position={labelPos}
					fontSize={0.12}
					color={color}
					anchorX="left"
					anchorY="bottom"
				>
					{groupName}
				</Text>
			)}
		</group>
	);
}

export function FixtureGroup({
	enableEditing,
	transformMode,
}: FixtureGroupProps) {
	const patchedFixtures = useFixtureStore((state) => state.patchedFixtures);
	const definitionsCache = useFixtureStore((state) => state.definitionsCache);
	const getDefinition = useFixtureStore((state) => state.getDefinition);
	const groups = useGroupStore((state) => state.groups);

	// Preload definitions for all patched fixtures (for bounding box calculation)
	useEffect(() => {
		if (!enableEditing) return;
		for (const fixture of patchedFixtures) {
			if (!definitionsCache.has(fixture.fixturePath)) {
				getDefinition(fixture.fixturePath);
			}
		}
	}, [enableEditing, patchedFixtures, definitionsCache, getDefinition]);

	// Compute bounding boxes for each group
	const boundingBoxes = useMemo(() => {
		if (!enableEditing) return [];

		const boxes: BoundingBox[] = [];
		const fixtureMap = new Map(patchedFixtures.map((f) => [f.id, f]));

		for (let i = 0; i < groups.length; i++) {
			const group = groups[i];
			if (group.fixtures.length === 0) continue;

			const min = new THREE.Vector3(Infinity, Infinity, Infinity);
			const max = new THREE.Vector3(-Infinity, -Infinity, -Infinity);

			for (const gf of group.fixtures) {
				const fixture = fixtureMap.get(gf.id);
				if (!fixture) continue;

				// Get definition from cache for actual dimensions
				const definition = definitionsCache.get(fixture.fixturePath);

				// Get rotated bounds for this fixture
				const bounds = getRotatedBounds(fixture, definition);
				min.min(bounds.min);
				max.max(bounds.max);
			}

			// Add padding
			const pad = 0.08;
			min.subScalar(pad);
			max.addScalar(pad);

			// Pick color based on group index
			const color = GROUP_COLORS[i % GROUP_COLORS.length];

			boxes.push({
				groupId: group.groupId,
				groupName: group.groupName,
				tags: group.tags,
				min: [min.x, min.y, min.z],
				max: [max.x, max.y, max.z],
				color,
			});
		}

		return boxes;
	}, [enableEditing, groups, patchedFixtures, definitionsCache]);

	return (
		<group>
			{patchedFixtures.map((fixture) => (
				<FixtureObject
					key={fixture.id}
					fixture={fixture}
					enableEditing={enableEditing}
					transformMode={transformMode}
				/>
			))}
			{enableEditing &&
				boundingBoxes.map((box) => (
					<GroupBoundingBox key={box.groupId} box={box} />
				))}
		</group>
	);
}
