import type { Object3D } from "three";
import { Box3, Vector3 } from "three";

export interface BoundingVolume {
	extents: { x: number; y: number; z: number };
	center: { x: number; y: number; z: number };
}

/**
 * Calculates the bounding volume of a 3D object and all its children.
 * Mirrors the approach used in QLC+ mainview3d.cpp:729-835
 */
export function calculateBoundingVolume(object: Object3D): BoundingVolume {
	const box = new Box3().setFromObject(object);

	const size = new Vector3();
	const center = new Vector3();

	box.getSize(size);
	box.getCenter(center);

	console.log("Bounding box calculation:", {
		min: box.min,
		max: box.max,
		size: size,
		objectScale: object.scale,
	});

	return {
		extents: {
			x: size.x,
			y: size.y,
			z: size.z,
		},
		center: {
			x: center.x,
			y: center.y,
			z: center.z,
		},
	};
}

/**
 * Calculates uniform scale factor based on physical dimensions.
 * Uses the minimum axis ratio to maintain aspect ratio.
 */
export function calculateUniformScale(
	desiredSize: { x: number; y: number; z: number },
	meshExtents: { x: number; y: number; z: number },
): number {
	const xScale = desiredSize.x / meshExtents.x;
	const yScale = desiredSize.y / meshExtents.y;
	const zScale = desiredSize.z / meshExtents.z;

	return Math.min(xScale, Math.min(yScale, zScale));
}

/**
 * Calculates per-axis scale factors so the model stretches to match
 * the fixture's physical dimensions exactly.
 */
export function calculatePerAxisScale(
	desiredSize: { x: number; y: number; z: number },
	meshExtents: { x: number; y: number; z: number },
): { x: number; y: number; z: number } {
	return {
		x: meshExtents.x > 0 ? desiredSize.x / meshExtents.x : 1,
		y: meshExtents.y > 0 ? desiredSize.y / meshExtents.y : 1,
		z: meshExtents.z > 0 ? desiredSize.z / meshExtents.z : 1,
	};
}

/**
 * Extracts physical dimensions from fixture definition and converts mm to meters.
 * Mirrors QLC+ mainview3d.cpp:959-966
 */
export function extractPhysicalDimensions(
	definition: Record<string, unknown>,
): { x: number; y: number; z: number } {
	const physicalData = definition?.Physical as Record<string, unknown>;
	const dimensions = physicalData?.Dimensions as Record<string, unknown>;

	// Default to 300mm if not defined
	const rawWidth =
		typeof dimensions?.["@Width"] === "number" ? dimensions["@Width"] : 0;
	const rawHeight =
		typeof dimensions?.["@Height"] === "number" ? dimensions["@Height"] : 0;
	const rawDepth =
		typeof dimensions?.["@Depth"] === "number" ? dimensions["@Depth"] : 0;

	const width = (rawWidth > 0 ? rawWidth : 300) / 1000;
	const height = (rawHeight > 0 ? rawHeight : 300) / 1000;
	const depth = (rawDepth > 0 ? rawDepth : 300) / 1000;

	return { x: width, y: height, z: depth };
}

/**
 * Applies per-axis scaling to a 3D object based on its physical dimensions.
 * The model stretches to match the fixture's width, height, and depth
 * so differently shaped par lights look correct.
 */
export function applyPhysicalDimensionScaling(
	object: Object3D,
	definition: Record<string, unknown>,
): {
	boundingVolume: BoundingVolume;
	scale: { x: number; y: number; z: number };
} {
	const desiredSize = extractPhysicalDimensions(definition);

	// Reset scale to get original mesh size
	object.scale.set(1, 1, 1);
	object.updateMatrixWorld(true);

	const boundingVolume = calculateBoundingVolume(object);

	const scale = calculatePerAxisScale(desiredSize, boundingVolume.extents);

	object.scale.set(scale.x, scale.y, scale.z);

	return { boundingVolume, scale };
}
