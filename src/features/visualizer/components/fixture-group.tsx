import { Line, Text, TransformControls } from "@react-three/drei";
import type React from "react";
import {
	useCallback,
	useEffect,
	useLayoutEffect,
	useMemo,
	useRef,
} from "react";
import * as THREE from "three";
import type { FixtureDefinition, PatchedFixture } from "@/bindings/fixtures";
import { useFixtureStore } from "../../universe/stores/use-fixture-store";
import { useGroupStore } from "../../universe/stores/use-group-store";
import { FixtureObject } from "./fixture-object";

interface FixtureGroupProps {
	enableEditing: boolean;
	transformMode: "translate" | "rotate";
	transformPivot: "individual" | "group";
	showBounds?: boolean;
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
	// Z-up (data) to Y-up (Three.js): swap Y↔Z
	const euler = new THREE.Euler(fixture.rotX, fixture.rotZ, fixture.rotY);
	const quaternion = new THREE.Quaternion().setFromEuler(euler);

	// Transform corners and find bounds
	const min = new THREE.Vector3(Infinity, Infinity, Infinity);
	const max = new THREE.Vector3(-Infinity, -Infinity, -Infinity);

	for (const corner of corners) {
		corner.applyQuaternion(quaternion);
		// Z-up (data) to Y-up (Three.js): swap Y↔Z
		corner.add(new THREE.Vector3(fixture.posX, fixture.posZ, fixture.posY));
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

/**
 * Renders a TransformControls gizmo at the centroid of all selected fixtures.
 * During drag, imperatively updates every selected fixture's Three.js group
 * so they all move/rotate in real-time. Persists final positions on mouseUp.
 */
function SelectionTransform({
	fixtureRefs,
	transformMode,
	transformPivot,
}: {
	fixtureRefs: { readonly current: Map<string, THREE.Group> };
	transformMode: "translate" | "rotate";
	transformPivot: "individual" | "group";
}) {
	const pivotRef = useRef<THREE.Group>(null);
	const isDragging = useRef(false);
	const dragStartPositions = useRef(
		new Map<string, { pos: THREE.Vector3; rot: THREE.Euler }>(),
	);
	const dragStartPivotPos = useRef(new THREE.Vector3());
	const dragStartPivotRot = useRef(new THREE.Euler());

	const selectedPatchedIds = useFixtureStore((s) => s.selectedPatchedIds);
	const patchedFixtures = useFixtureStore((s) => s.patchedFixtures);
	const moveFixtureSpatial = useFixtureStore((s) => s.moveFixtureSpatial);

	const selectedFixtures = useMemo(
		() => patchedFixtures.filter((f) => selectedPatchedIds.has(f.id)),
		[patchedFixtures, selectedPatchedIds],
	);

	// Compute centroid in Three.js coords (Y-up)
	const centroid = useMemo(() => {
		if (selectedFixtures.length === 0) return new THREE.Vector3();
		const c = new THREE.Vector3();
		for (const f of selectedFixtures) {
			c.x += f.posX;
			c.y += f.posZ; // data Z → Three.js Y
			c.z += f.posY; // data Y → Three.js Z
		}
		c.divideScalar(selectedFixtures.length);
		return c;
	}, [selectedFixtures]);

	// Position pivot at centroid (imperatively so drag doesn't get reset)
	useLayoutEffect(() => {
		if (pivotRef.current && !isDragging.current) {
			pivotRef.current.position.copy(centroid);
			pivotRef.current.rotation.set(0, 0, 0);
		}
	}, [centroid]);

	if (selectedFixtures.length < 2) return null;

	return (
		<>
			<group ref={pivotRef} />
			<TransformControls
				object={pivotRef as React.RefObject<THREE.Group>}
				mode={transformMode}
				rotationSnap={
					transformMode === "rotate" ? THREE.MathUtils.degToRad(15) : undefined
				}
				onMouseDown={() => {
					isDragging.current = true;
					if (pivotRef.current) {
						dragStartPivotPos.current.copy(pivotRef.current.position);
						dragStartPivotRot.current.copy(pivotRef.current.rotation);
					}
					dragStartPositions.current.clear();
					const refs = fixtureRefs.current;
					for (const f of selectedFixtures) {
						const ref = refs.get(f.id);
						if (ref) {
							dragStartPositions.current.set(f.id, {
								pos: ref.position.clone(),
								rot: ref.rotation.clone(),
							});
						}
					}
				}}
				onChange={() => {
					if (!isDragging.current || !pivotRef.current) return;
					const refs = fixtureRefs.current;

					if (transformMode === "translate") {
						const delta = pivotRef.current.position
							.clone()
							.sub(dragStartPivotPos.current);
						for (const [id, start] of dragStartPositions.current) {
							const ref = refs.get(id);
							if (ref) {
								ref.position.copy(start.pos.clone().add(delta));
							}
						}
					} else if (transformMode === "rotate") {
						const pivotRot = pivotRef.current.rotation;
						const startRot = dragStartPivotRot.current;

						if (transformPivot === "group") {
							// Group: orbit around centroid + rotate each
							const deltaQuat = new THREE.Quaternion().setFromEuler(
								new THREE.Euler(
									pivotRot.x - startRot.x,
									pivotRot.y - startRot.y,
									pivotRot.z - startRot.z,
								),
							);
							for (const [id, start] of dragStartPositions.current) {
								const ref = refs.get(id);
								if (ref) {
									const offset = start.pos
										.clone()
										.sub(dragStartPivotPos.current);
									offset.applyQuaternion(deltaQuat);
									ref.position.copy(dragStartPivotPos.current).add(offset);
									ref.rotation.set(
										start.rot.x - (pivotRot.x - startRot.x),
										start.rot.y - (pivotRot.y - startRot.y),
										start.rot.z - (pivotRot.z - startRot.z),
									);
								}
							}
						} else {
							// Individual: rotate each in place
							for (const [id, start] of dragStartPositions.current) {
								const ref = refs.get(id);
								if (ref) {
									ref.rotation.set(
										start.rot.x + pivotRot.x - startRot.x,
										start.rot.y + pivotRot.y - startRot.y,
										start.rot.z + pivotRot.z - startRot.z,
									);
								}
							}
						}
					}
				}}
				onMouseUp={() => {
					isDragging.current = false;
					const refs = fixtureRefs.current;

					// Persist all selected fixtures' final positions
					for (const f of selectedFixtures) {
						const ref = refs.get(f.id);
						if (ref) {
							// Y-up (Three.js) to Z-up (data): swap Y↔Z
							moveFixtureSpatial(
								f.id,
								{
									x: ref.position.x,
									y: ref.position.z,
									z: ref.position.y,
								},
								{
									x: ref.rotation.x,
									y: ref.rotation.z,
									z: ref.rotation.y,
								},
							);
						}
					}
				}}
			/>
		</>
	);
}

export function FixtureGroup({
	enableEditing,
	transformMode,
	transformPivot,
	showBounds = false,
}: FixtureGroupProps) {
	const patchedFixtures = useFixtureStore((state) => state.patchedFixtures);
	const definitionsCache = useFixtureStore((state) => state.definitionsCache);
	const getDefinition = useFixtureStore((state) => state.getDefinition);
	const groups = useGroupStore((state) => state.groups);

	// Ref registry for imperative multi-selection transforms
	const fixtureRefsMap = useRef(new Map<string, THREE.Group>());
	const registerFixtureRef = useCallback(
		(id: string, ref: THREE.Group | null) => {
			if (ref) {
				fixtureRefsMap.current.set(id, ref);
			} else {
				fixtureRefsMap.current.delete(id);
			}
		},
		[],
	);

	// Preload definitions for all patched fixtures (for bounding box calculation)
	useEffect(() => {
		if (!showBounds) return;
		for (const fixture of patchedFixtures) {
			if (!definitionsCache.has(fixture.fixturePath)) {
				getDefinition(fixture.fixturePath);
			}
		}
	}, [showBounds, patchedFixtures, definitionsCache, getDefinition]);

	// Compute bounding boxes for each group
	const boundingBoxes = useMemo(() => {
		if (!showBounds) return [];

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
	}, [showBounds, groups, patchedFixtures, definitionsCache]);

	return (
		<group>
			{patchedFixtures.map((fixture) => (
				<FixtureObject
					key={fixture.id}
					fixture={fixture}
					enableEditing={enableEditing}
					transformMode={transformMode}
					onGroupRef={registerFixtureRef}
				/>
			))}
			{enableEditing && (
				<SelectionTransform
					fixtureRefs={fixtureRefsMap}
					transformMode={transformMode}
					transformPivot={transformPivot}
				/>
			)}
			{showBounds &&
				boundingBoxes.map((box) => (
					<GroupBoundingBox key={box.groupId} box={box} />
				))}
		</group>
	);
}
