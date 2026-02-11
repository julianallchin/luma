import { Html, Line } from "@react-three/drei";
import { useMemo } from "react";
import { Vector3 } from "three";
import { useFixtureStore } from "../../universe/stores/use-fixture-store";

interface MirrorPairResult {
	centerX: number;
	pairs: { indexA: number; indexB: number; error: number }[];
	unmatched: number[];
	planeExtents: { minY: number; maxY: number; minZ: number; maxZ: number };
}

/**
 * Find mirror pairs across the X axis.
 * For each fixture, its expected mirror position is (2*centerX - posX, posY, posZ).
 * Greedy matching: outermost fixtures first, closest match wins.
 */
function findMirrorPairs(
	fixtures: { id: string; posX: number; posY: number; posZ: number }[],
): MirrorPairResult {
	const n = fixtures.length;

	// Compute center X as mean of all fixture X positions
	let sumX = 0;
	for (const f of fixtures) sumX += f.posX;
	let centerX = sumX / n;
	// Snap to 0 if close
	if (Math.abs(centerX) < 0.1) centerX = 0;

	// Sort indices by distance from center (outermost first)
	const indices = Array.from({ length: n }, (_, i) => i);
	indices.sort(
		(a, b) =>
			Math.abs(fixtures[b].posX - centerX) -
			Math.abs(fixtures[a].posX - centerX),
	);

	const matched = new Set<number>();
	const pairs: { indexA: number; indexB: number; error: number }[] = [];
	const threshold = 0.5;

	for (const i of indices) {
		if (matched.has(i)) continue;

		const f = fixtures[i];
		const mirrorX = 2 * centerX - f.posX;
		const mirrorY = f.posY;
		const mirrorZ = f.posZ;

		let bestJ = -1;
		let bestDist = threshold;

		for (const j of indices) {
			if (j === i || matched.has(j)) continue;
			const g = fixtures[j];
			const dx = g.posX - mirrorX;
			const dy = g.posY - mirrorY;
			const dz = g.posZ - mirrorZ;
			const dist = Math.sqrt(dx * dx + dy * dy + dz * dz);
			if (dist < bestDist) {
				bestDist = dist;
				bestJ = j;
			}
		}

		if (bestJ >= 0) {
			matched.add(i);
			matched.add(bestJ);
			pairs.push({ indexA: i, indexB: bestJ, error: bestDist });
		}
	}

	const unmatched = indices.filter((i) => !matched.has(i));

	// Compute plane extents from all fixtures
	let minY = Infinity,
		maxY = -Infinity,
		minZ = Infinity,
		maxZ = -Infinity;
	for (const f of fixtures) {
		if (f.posY < minY) minY = f.posY;
		if (f.posY > maxY) maxY = f.posY;
		if (f.posZ < minZ) minZ = f.posZ;
		if (f.posZ > maxZ) maxZ = f.posZ;
	}

	return {
		centerX,
		pairs,
		unmatched,
		planeExtents: { minY, maxY, minZ, maxZ },
	};
}

/**
 * Color a pair line by YZ error: cyan (good) → yellow (decent) → red (poor)
 */
function errorColor(error: number): string {
	if (error < 0.1) return "#00ffff"; // cyan
	if (error < 0.3) return "#ffff00"; // yellow
	return "#ff4444"; // red
}

/**
 * Debug visualization for mirror pairs across the X axis.
 */
export function MirrorDebug() {
	const patchedFixtures = useFixtureStore((state) => state.patchedFixtures);

	const result = useMemo(() => {
		if (patchedFixtures.length < 2) return null;

		return findMirrorPairs(
			patchedFixtures.map((f) => ({
				id: f.id,
				posX: f.posX,
				posY: f.posY,
				posZ: f.posZ,
			})),
		);
	}, [patchedFixtures]);

	if (!result) return null;

	const { centerX, pairs, unmatched, planeExtents } = result;

	// Coordinate transform: Three.js = [posX, posZ, posY]
	const toThree = (posX: number, posY: number, posZ: number) =>
		new Vector3(posX, posZ, posY);

	// Mirror plane dimensions (with padding)
	const padding = 0.5;
	const planeHeight = planeExtents.maxZ - planeExtents.minZ + padding * 2;
	const planeDepth = planeExtents.maxY - planeExtents.minY + padding * 2;
	const planeCenterZ = (planeExtents.minZ + planeExtents.maxZ) / 2;
	const planeCenterY = (planeExtents.minY + planeExtents.maxY) / 2;
	// In Three.js coords: plane at x=centerX, y=planeCenterZ (height), z=planeCenterY (depth)
	const planePos = new Vector3(centerX, planeCenterZ, planeCenterY);

	// Stats
	const avgError =
		pairs.length > 0
			? pairs.reduce((s, p) => s + p.error, 0) / pairs.length
			: 0;

	// Build pair index map: fixture index → pair number
	const pairIndexMap = new Map<number, number>();
	for (let pi = 0; pi < pairs.length; pi++) {
		pairIndexMap.set(pairs[pi].indexA, pi);
		pairIndexMap.set(pairs[pi].indexB, pi);
	}

	return (
		<group>
			{/* Mirror plane — semi-transparent orange, rotated to face along X */}
			<mesh position={planePos} rotation={[0, Math.PI / 2, 0]}>
				<planeGeometry args={[planeDepth, planeHeight]} />
				<meshBasicMaterial
					color="#ff8800"
					transparent
					opacity={0.08}
					side={2}
					depthWrite={false}
				/>
			</mesh>

			{/* Wireframe border for the plane */}
			<mesh position={planePos} rotation={[0, Math.PI / 2, 0]}>
				<planeGeometry args={[planeDepth, planeHeight]} />
				<meshBasicMaterial
					color="#ff8800"
					wireframe
					transparent
					opacity={0.3}
					side={2}
				/>
			</mesh>

			{/* Pair lines */}
			{pairs.map((pair) => {
				const fA = patchedFixtures[pair.indexA];
				const fB = patchedFixtures[pair.indexB];
				const posA = toThree(fA.posX, fA.posY, fA.posZ);
				const posB = toThree(fB.posX, fB.posY, fB.posZ);
				const color = errorColor(pair.error);

				// Midpoint (where line crosses mirror plane)
				const mid = posA.clone().lerp(posB, 0.5);

				return (
					<group key={`pair-${pair.indexA}-${pair.indexB}`}>
						<Line
							points={[posA, posB]}
							color={color}
							lineWidth={2}
							opacity={0.8}
							transparent
						/>
						{/* Midpoint dot — small orange sphere */}
						<mesh position={mid}>
							<sphereGeometry args={[0.03, 8, 8]} />
							<meshBasicMaterial color="#ff8800" />
						</mesh>
					</group>
				);
			})}

			{/* Unmatched markers — red wireframe octahedrons */}
			{unmatched.map((idx) => {
				const f = patchedFixtures[idx];
				const pos = toThree(f.posX, f.posY, f.posZ);
				return (
					<mesh key={`unmatched-${idx}`} position={pos}>
						<octahedronGeometry args={[0.12]} />
						<meshBasicMaterial color="#ff4444" wireframe />
					</mesh>
				);
			})}

			{/* Pair index labels above each fixture */}
			{patchedFixtures.map((f, idx) => {
				const pairNum = pairIndexMap.get(idx);
				if (pairNum === undefined) return null;
				const pos = toThree(f.posX, f.posY, f.posZ);
				return (
					<Html
						key={`label-${f.id}`}
						position={pos.clone().add(new Vector3(0, 0.15, 0))}
						center
						style={{ pointerEvents: "none" }}
					>
						<div className="rounded bg-black/80 px-1.5 py-0.5 text-[10px] font-mono text-orange-400 whitespace-nowrap">
							{pairNum}
						</div>
					</Html>
				);
			})}

			{/* Stats panel at plane center */}
			<Html position={planePos} style={{ pointerEvents: "none" }}>
				<div className="ml-4 rounded bg-black/90 px-2 py-1 text-[10px] font-mono text-white whitespace-nowrap border border-orange-500/50">
					<div className="text-orange-400 font-bold mb-1">Mirror Debug</div>
					<div>center X: {centerX.toFixed(3)}m</div>
					<div>
						pairs: <span className="text-cyan-400">{pairs.length}</span>
						{unmatched.length > 0 && (
							<>
								{" / unmatched: "}
								<span className="text-red-400">{unmatched.length}</span>
							</>
						)}
					</div>
					<div>avg error: {avgError.toFixed(4)}m</div>
				</div>
			</Html>
		</group>
	);
}
