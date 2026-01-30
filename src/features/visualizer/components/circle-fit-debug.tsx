import { Html, Line } from "@react-three/drei";
import { useMemo } from "react";
import { Vector3 } from "three";
import { useFixtureStore } from "../../universe/stores/use-fixture-store";

interface CircleFitResult {
	// Fitted circle in 3D space
	center: Vector3;
	radius: number;
	normal: Vector3; // Plane normal
	// Basis vectors for the plane
	basisU: Vector3;
	basisV: Vector3;
	// Per-fixture data
	fixtures: {
		id: string;
		position: Vector3;
		angularPosition: number; // 0..1
		distanceFromCircle: number; // Error metric
		isInlier: boolean; // Whether this fixture was used in the fit
	}[];
}

/**
 * Fit a circle through exactly 3 points (for RANSAC)
 * Returns null if points are collinear
 */
function fitCircleThrough3Points(
	p1: { u: number; v: number },
	p2: { u: number; v: number },
	p3: { u: number; v: number },
): { a: number; b: number; r: number } | null {
	// Using the circumcenter formula
	const ax = p1.u, ay = p1.v;
	const bx = p2.u, by = p2.v;
	const cx = p3.u, cy = p3.v;

	const d = 2 * (ax * (by - cy) + bx * (cy - ay) + cx * (ay - by));
	if (Math.abs(d) < 1e-10) return null; // Collinear

	const aSq = ax * ax + ay * ay;
	const bSq = bx * bx + by * by;
	const cSq = cx * cx + cy * cy;

	const centerU = (aSq * (by - cy) + bSq * (cy - ay) + cSq * (ay - by)) / d;
	const centerV = (aSq * (cx - bx) + bSq * (ax - cx) + cSq * (bx - ax)) / d;

	const r = Math.sqrt((ax - centerU) ** 2 + (ay - centerV) ** 2);

	return { a: centerU, b: centerV, r };
}

/**
 * Kåsa circle fit on a subset of points
 */
function kasaFit(
	points: { u: number; v: number }[],
): { a: number; b: number; r: number } | null {
	const n = points.length;
	if (n < 3) return null;

	let sumU = 0,
		sumV = 0,
		sumUU = 0,
		sumVV = 0,
		sumUV = 0;
	let sumUUU = 0,
		sumVVV = 0,
		sumUUV = 0,
		sumUVV = 0;

	for (const p of points) {
		const { u, v } = p;
		sumU += u;
		sumV += v;
		sumUU += u * u;
		sumVV += v * v;
		sumUV += u * v;
		sumUUU += u * u * u;
		sumVVV += v * v * v;
		sumUUV += u * u * v;
		sumUVV += u * v * v;
	}

	const ATA = [
		[4 * sumUU, 4 * sumUV, 2 * sumU],
		[4 * sumUV, 4 * sumVV, 2 * sumV],
		[2 * sumU, 2 * sumV, n],
	];

	const ATz = [
		2 * (sumUUU + sumUVV),
		2 * (sumUUV + sumVVV),
		sumUU + sumVV,
	];

	const solution = solve3x3(ATA, ATz);
	if (!solution) return null;

	const [a, b, c] = solution;
	const radiusSq = a * a + b * b + c;
	if (radiusSq <= 0) return null;

	return { a, b, r: Math.sqrt(radiusSq) };
}

/**
 * RANSAC circle fitting with outlier rejection
 */
function ransacCircleFit(
	points: { id: string; u: number; v: number; original: Vector3 }[],
	iterations = 100,
	inlierThreshold = 2.5, // Distance threshold to be considered inlier (meters)
): { fit: { a: number; b: number; r: number }; inlierIndices: Set<number> } | null {
	const n = points.length;
	if (n < 3) return null;

	let bestFit: { a: number; b: number; r: number } | null = null;
	let bestInliers = new Set<number>();

	// If we only have 3 points, just fit them directly
	if (n === 3) {
		const fit = fitCircleThrough3Points(points[0], points[1], points[2]);
		if (fit) {
			return { fit, inlierIndices: new Set([0, 1, 2]) };
		}
		return null;
	}

	for (let iter = 0; iter < iterations; iter++) {
		// Pick 3 random distinct points
		const indices: number[] = [];
		while (indices.length < 3) {
			const idx = Math.floor(Math.random() * n);
			if (!indices.includes(idx)) {
				indices.push(idx);
			}
		}

		// Fit circle through these 3 points
		const fit = fitCircleThrough3Points(
			points[indices[0]],
			points[indices[1]],
			points[indices[2]],
		);
		if (!fit) continue;

		// Count inliers
		const inliers = new Set<number>();
		for (let i = 0; i < n; i++) {
			const dist = Math.abs(
				Math.sqrt((points[i].u - fit.a) ** 2 + (points[i].v - fit.b) ** 2) - fit.r,
			);
			if (dist < inlierThreshold) {
				inliers.add(i);
			}
		}

		// Keep if better
		if (inliers.size > bestInliers.size) {
			bestInliers = inliers;
			bestFit = fit;
		}

		// Early exit if we have enough inliers (e.g., 90%)
		if (inliers.size >= n * 0.9) break;
	}

	if (!bestFit || bestInliers.size < 3) return null;

	// Refit using only inliers with Kåsa method for better accuracy
	const inlierPoints = [...bestInliers].map((i) => points[i]);
	const refinedFit = kasaFit(inlierPoints);

	if (refinedFit) {
		// Recompute inliers with refined fit
		const refinedInliers = new Set<number>();
		for (let i = 0; i < n; i++) {
			const dist = Math.abs(
				Math.sqrt((points[i].u - refinedFit.a) ** 2 + (points[i].v - refinedFit.b) ** 2) -
					refinedFit.r,
			);
			if (dist < inlierThreshold) {
				refinedInliers.add(i);
			}
		}
		return { fit: refinedFit, inlierIndices: refinedInliers };
	}

	return { fit: bestFit, inlierIndices: bestInliers };
}

/**
 * PCA-based plane fitting + RANSAC circle fit with outlier rejection
 */
function fitCircle3D(
	positions: { id: string; x: number; y: number; z: number }[],
): CircleFitResult | null {
	const n = positions.length;
	if (n < 3) return null;

	// 1. Compute centroid
	let cx = 0,
		cy = 0,
		cz = 0;
	for (const p of positions) {
		cx += p.x;
		cy += p.y;
		cz += p.z;
	}
	cx /= n;
	cy /= n;
	cz /= n;
	const centroid = new Vector3(cx, cy, cz);

	// 2. Build covariance matrix (3x3, symmetric)
	let cxx = 0,
		cxy = 0,
		cxz = 0;
	let cyy = 0,
		cyz = 0;
	let czz = 0;

	for (const p of positions) {
		const dx = p.x - cx;
		const dy = p.y - cy;
		const dz = p.z - cz;
		cxx += dx * dx;
		cxy += dx * dy;
		cxz += dx * dz;
		cyy += dy * dy;
		cyz += dy * dz;
		czz += dz * dz;
	}

	// 3. Find eigenvectors using power iteration
	const cov = [
		[cxx, cxy, cxz],
		[cxy, cyy, cyz],
		[cxz, cyz, czz],
	];

	const { normal, basisU, basisV } = findPlaneBasis(cov);

	// 4. Project points onto plane (2D coordinates)
	const projected: { id: string; u: number; v: number; original: Vector3 }[] =
		[];
	for (const p of positions) {
		const dx = p.x - cx;
		const dy = p.y - cy;
		const dz = p.z - cz;
		const u = dx * basisU.x + dy * basisU.y + dz * basisU.z;
		const v = dx * basisV.x + dy * basisV.y + dz * basisV.z;
		projected.push({ id: p.id, u, v, original: new Vector3(p.x, p.y, p.z) });
	}

	// 5. RANSAC circle fit with outlier rejection
	const ransacResult = ransacCircleFit(projected);

	if (!ransacResult) {
		return fallbackToCentroid(positions, centroid, normal, basisU, basisV);
	}

	const { fit, inlierIndices } = ransacResult;
	const { a, b, r: radius } = fit;

	// Check for degenerate fit (radius too large = nearly collinear)
	const boundingRadius = Math.max(
		...projected.map((p) => Math.sqrt(p.u * p.u + p.v * p.v)),
	);
	if (radius > boundingRadius * 10) {
		return fallbackToCentroid(positions, centroid, normal, basisU, basisV);
	}

	// 6. Convert 2D circle center back to 3D
	const center3D = centroid
		.clone()
		.addScaledVector(basisU, a)
		.addScaledVector(basisV, b);

	// 7. Compute angular positions for ALL fixtures (including outliers)
	const fixtures = projected.map((p, idx) => {
		const du = p.u - a;
		const dv = p.v - b;
		const angle = Math.atan2(dv, du); // -PI to PI
		const angularPosition = (angle + Math.PI) / (2 * Math.PI); // 0..1
		const distFromCenter = Math.sqrt(du * du + dv * dv);
		const distanceFromCircle = Math.abs(distFromCenter - radius);

		return {
			id: p.id,
			position: p.original,
			angularPosition,
			distanceFromCircle,
			isInlier: inlierIndices.has(idx),
		};
	});

	return {
		center: center3D,
		radius,
		normal,
		basisU,
		basisV,
		fixtures,
	};
}

function fallbackToCentroid(
	positions: { id: string; x: number; y: number; z: number }[],
	centroid: Vector3,
	normal: Vector3,
	basisU: Vector3,
	basisV: Vector3,
): CircleFitResult {
	// Use centroid as center, average distance as radius
	let sumDist = 0;
	const projected: { id: string; u: number; v: number; original: Vector3 }[] =
		[];

	for (const p of positions) {
		const dx = p.x - centroid.x;
		const dy = p.y - centroid.y;
		const dz = p.z - centroid.z;
		const u = dx * basisU.x + dy * basisU.y + dz * basisU.z;
		const v = dx * basisV.x + dy * basisV.y + dz * basisV.z;
		sumDist += Math.sqrt(u * u + v * v);
		projected.push({ id: p.id, u, v, original: new Vector3(p.x, p.y, p.z) });
	}

	const radius = sumDist / positions.length;

	const fixtures = projected.map((p) => {
		const angle = Math.atan2(p.v, p.u);
		const angularPosition = (angle + Math.PI) / (2 * Math.PI);
		const dist = Math.sqrt(p.u * p.u + p.v * p.v);
		return {
			id: p.id,
			position: p.original,
			angularPosition,
			distanceFromCircle: Math.abs(dist - radius),
			isInlier: true, // Fallback treats all as inliers
		};
	});

	return {
		center: centroid,
		radius,
		normal,
		basisU,
		basisV,
		fixtures,
	};
}

/**
 * Find plane basis vectors from covariance matrix
 * Returns normal (smallest eigenvector) and two basis vectors
 */
function findPlaneBasis(cov: number[][]): {
	normal: Vector3;
	basisU: Vector3;
	basisV: Vector3;
} {
	// Simple power iteration to find eigenvectors
	// For robustness, we use a simple approach:
	// 1. Find the dominant eigenvector
	// 2. Deflate and find second
	// 3. Cross product for third (normal)

	const v1 = powerIteration(cov);
	const cov2 = deflate(cov, v1);
	const v2 = powerIteration(cov2);

	// Normal is perpendicular to both
	const normal = new Vector3().crossVectors(
		new Vector3(v1[0], v1[1], v1[2]),
		new Vector3(v2[0], v2[1], v2[2]),
	);

	// Handle degenerate case (all points collinear or coincident)
	if (normal.length() < 1e-10) {
		// Default to XY plane
		return {
			normal: new Vector3(0, 0, 1),
			basisU: new Vector3(1, 0, 0),
			basisV: new Vector3(0, 1, 0),
		};
	}

	normal.normalize();

	const basisU = new Vector3(v1[0], v1[1], v1[2]).normalize();
	const basisV = new Vector3().crossVectors(normal, basisU).normalize();

	return { normal, basisU, basisV };
}

function powerIteration(matrix: number[][], iterations = 20): number[] {
	let v = [1, 0, 0];

	for (let i = 0; i < iterations; i++) {
		// Multiply
		const newV = [
			matrix[0][0] * v[0] + matrix[0][1] * v[1] + matrix[0][2] * v[2],
			matrix[1][0] * v[0] + matrix[1][1] * v[1] + matrix[1][2] * v[2],
			matrix[2][0] * v[0] + matrix[2][1] * v[1] + matrix[2][2] * v[2],
		];

		// Normalize
		const len = Math.sqrt(newV[0] ** 2 + newV[1] ** 2 + newV[2] ** 2);
		if (len > 1e-10) {
			v = [newV[0] / len, newV[1] / len, newV[2] / len];
		}
	}

	return v;
}

function deflate(matrix: number[][], v: number[]): number[][] {
	// Compute eigenvalue (Rayleigh quotient)
	const Av = [
		matrix[0][0] * v[0] + matrix[0][1] * v[1] + matrix[0][2] * v[2],
		matrix[1][0] * v[0] + matrix[1][1] * v[1] + matrix[1][2] * v[2],
		matrix[2][0] * v[0] + matrix[2][1] * v[1] + matrix[2][2] * v[2],
	];
	const lambda = v[0] * Av[0] + v[1] * Av[1] + v[2] * Av[2];

	// Subtract outer product: M - λ * v * v^T
	return [
		[
			matrix[0][0] - lambda * v[0] * v[0],
			matrix[0][1] - lambda * v[0] * v[1],
			matrix[0][2] - lambda * v[0] * v[2],
		],
		[
			matrix[1][0] - lambda * v[1] * v[0],
			matrix[1][1] - lambda * v[1] * v[1],
			matrix[1][2] - lambda * v[1] * v[2],
		],
		[
			matrix[2][0] - lambda * v[2] * v[0],
			matrix[2][1] - lambda * v[2] * v[1],
			matrix[2][2] - lambda * v[2] * v[2],
		],
	];
}

function solve3x3(A: number[][], b: number[]): number[] | null {
	// Cramer's rule for 3x3
	const det =
		A[0][0] * (A[1][1] * A[2][2] - A[1][2] * A[2][1]) -
		A[0][1] * (A[1][0] * A[2][2] - A[1][2] * A[2][0]) +
		A[0][2] * (A[1][0] * A[2][1] - A[1][1] * A[2][0]);

	if (Math.abs(det) < 1e-10) return null;

	const detX =
		b[0] * (A[1][1] * A[2][2] - A[1][2] * A[2][1]) -
		A[0][1] * (b[1] * A[2][2] - A[1][2] * b[2]) +
		A[0][2] * (b[1] * A[2][1] - A[1][1] * b[2]);

	const detY =
		A[0][0] * (b[1] * A[2][2] - A[1][2] * b[2]) -
		b[0] * (A[1][0] * A[2][2] - A[1][2] * A[2][0]) +
		A[0][2] * (A[1][0] * b[2] - b[1] * A[2][0]);

	const detZ =
		A[0][0] * (A[1][1] * b[2] - b[1] * A[2][1]) -
		A[0][1] * (A[1][0] * b[2] - b[1] * A[2][0]) +
		b[0] * (A[1][0] * A[2][1] - A[1][1] * A[2][0]);

	return [detX / det, detY / det, detZ / det];
}

/**
 * Generate points along the fitted circle for visualization
 */
function generateCirclePoints(
	fit: CircleFitResult,
	segments = 64,
): Vector3[] {
	const points: Vector3[] = [];

	for (let i = 0; i <= segments; i++) {
		const angle = (i / segments) * Math.PI * 2;
		const u = Math.cos(angle) * fit.radius;
		const v = Math.sin(angle) * fit.radius;

		const point = fit.center
			.clone()
			.addScaledVector(fit.basisU, u)
			.addScaledVector(fit.basisV, v);

		points.push(point);
	}

	return points;
}

/**
 * Debug visualization component - add inside the Canvas
 */
export function CircleFitDebug() {
	const patchedFixtures = useFixtureStore((state) => state.patchedFixtures);

	const fit = useMemo(() => {
		if (patchedFixtures.length < 3) return null;

		// Map from data coords (Z-up) to Three.js coords (Y-up)
		// Three.js X = data X
		// Three.js Y = data Z (up)
		// Three.js Z = data Y (forward)
		const positions = patchedFixtures.map((f) => ({
			id: f.id,
			x: f.posX,
			y: f.posZ, // data Z -> Three.js Y
			z: f.posY, // data Y -> Three.js Z
		}));

		return fitCircle3D(positions);
	}, [patchedFixtures]);

	if (!fit) return null;

	const circlePoints = generateCirclePoints(fit);

	// Sort INLIER fixtures by angular position for the connecting line
	const inlierFixtures = fit.fixtures.filter((f) => f.isInlier);
	const outlierFixtures = fit.fixtures.filter((f) => !f.isInlier);

	const sortedInliers = [...inlierFixtures].sort(
		(a, b) => a.angularPosition - b.angularPosition,
	);

	// Create line connecting inlier fixtures in angular order
	const fixtureOrderPoints = [
		...sortedInliers.map((f) => f.position),
		sortedInliers[0]?.position, // Close the loop
	].filter(Boolean) as Vector3[];

	return (
		<group>
			{/* Fitted circle */}
			<Line
				points={circlePoints}
				color="#00ff88"
				lineWidth={2}
				opacity={0.7}
				transparent
			/>

			{/* Center point */}
			<mesh position={fit.center}>
				<sphereGeometry args={[0.05, 16, 16]} />
				<meshBasicMaterial color="#ff0088" />
			</mesh>

			{/* Stats panel at center */}
			<Html position={fit.center} style={{ pointerEvents: "none" }}>
				<div className="ml-4 rounded bg-black/90 px-2 py-1 text-[10px] font-mono text-white whitespace-nowrap border border-green-500/50">
					<div className="text-green-400 font-bold mb-1">Circle Fit (RANSAC)</div>
					<div>radius: {fit.radius.toFixed(3)}m</div>
					<div>
						inliers:{" "}
						<span className="text-green-400">{inlierFixtures.length}</span>
						{outlierFixtures.length > 0 && (
							<>
								{" / outliers: "}
								<span className="text-red-400">{outlierFixtures.length}</span>
							</>
						)}
					</div>
					<div>
						avg error:{" "}
						{inlierFixtures.length > 0
							? (
									inlierFixtures.reduce((s, f) => s + f.distanceFromCircle, 0) /
									inlierFixtures.length
								).toFixed(4)
							: "N/A"}
						m
					</div>
				</div>
			</Html>

			{/* Lines from center to inlier fixtures */}
			{inlierFixtures.map((f) => (
				<Line
					key={`radial-${f.id}`}
					points={[fit.center, f.position]}
					color="#ffff00"
					lineWidth={1}
					opacity={0.3}
					transparent
				/>
			))}

			{/* Lines from center to outlier fixtures (red, dashed-style) */}
			{outlierFixtures.map((f) => (
				<Line
					key={`radial-outlier-${f.id}`}
					points={[fit.center, f.position]}
					color="#ff4444"
					lineWidth={1}
					opacity={0.5}
					transparent
				/>
			))}

			{/* Line connecting fixtures in angular order */}
			{fixtureOrderPoints.length > 1 && (
				<Line
					points={fixtureOrderPoints}
					color="#00ffff"
					lineWidth={2}
					opacity={0.8}
					transparent
				/>
			)}

			{/* Normal vector (for debugging plane orientation) */}
			<Line
				points={[
					fit.center,
					fit.center.clone().addScaledVector(fit.normal, 0.5),
				]}
				color="#ff00ff"
				lineWidth={2}
			/>

			{/* Angular position labels - inliers green, outliers red */}
			{fit.fixtures.map((f) => (
				<Html
					key={`label-${f.id}`}
					position={f.position.clone().add(new Vector3(0, 0.15, 0))}
					center
					style={{ pointerEvents: "none" }}
				>
					<div
						className={`rounded bg-black/80 px-1.5 py-0.5 text-[10px] font-mono whitespace-nowrap ${
							f.isInlier ? "text-green-400" : "text-red-400 line-through"
						}`}
					>
						{f.angularPosition.toFixed(3)}
						{!f.isInlier && " ✗"}
					</div>
				</Html>
			))}
		</group>
	);
}

// Export the algorithm for testing/reuse
export { fitCircle3D, type CircleFitResult };
