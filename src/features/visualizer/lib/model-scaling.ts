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
 * Mirrors QLC+ mainview3d.cpp:1524-1546 logic:
 * - Calculate scale ratio for each axis
 * - Use the minimum ratio to maintain aspect ratio
 * - Returns the uniform scale factor
 */
export function calculateUniformScale(
	desiredSize: { x: number; y: number; z: number },
	meshExtents: { x: number; y: number; z: number },
): number {
	const xScale = desiredSize.x / meshExtents.x;
	const yScale = desiredSize.y / meshExtents.y;
	const zScale = desiredSize.z / meshExtents.z;

	// Use minimum scale to maintain aspect ratio
	return Math.min(xScale, Math.min(yScale, zScale));
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
 * Applies scaling to a 3D object based on its physical dimensions.
 * Mirrors the complete scaling pipeline from QLC+:
 * 1. Extract physical dimensions from fixture definition
 * 2. Calculate mesh bounding volume
 * 3. Calculate uniform scale factor
 * 4. Apply scale to root object
 */
export function applyPhysicalDimensionScaling(
	object: Object3D,
	definition: Record<string, unknown>,
): { boundingVolume: BoundingVolume; scale: number } {
	// Step 1: Get desired dimensions from fixture definition
	const desiredSize = extractPhysicalDimensions(definition);

	// Step 2: Calculate actual mesh bounding volume (BEFORE any scaling)
	// Reset scale to 1.0 first to get original mesh size
	object.scale.set(1, 1, 1);
	object.updateMatrixWorld(true);

	const boundingVolume = calculateBoundingVolume(object);

	// Step 3: Calculate uniform scale factor
	const scale = calculateUniformScale(desiredSize, boundingVolume.extents);

	console.log("Scaling fixture:", {
		desiredSize,
		meshExtents: boundingVolume.extents,
		scaleRatios: {
			x: desiredSize.x / boundingVolume.extents.x,
			y: desiredSize.y / boundingVolume.extents.y,
			z: desiredSize.z / boundingVolume.extents.z,
		},
		finalScale: scale,
	});

	// Step 4: Apply scale to the object
	object.scale.set(scale, scale, scale);

	return { boundingVolume, scale };
}
