import {
	BaseEdge,
	type ConnectionLineComponentProps,
	type EdgeProps,
	Position,
} from "reactflow";

// Wire shape: a short horizontal stub out of each port, joined by one
// straight diagonal, with the two corners (stub→diagonal) rounded by a
// fixed-radius arc fillet.
const STUB = 16; // px length of the horizontal stub leaving each port
const RADIUS = 10; // px corner fillet radius

type Pt = { x: number; y: number };

// Horizontal direction a stub leaves a port on the given side.
function stubDir(position: Position): number {
	if (position === Position.Left) return -1;
	if (position === Position.Right) return 1;
	return 0;
}

// Pull a point `r` away from vertex `v` toward `t`.
function trim(v: Pt, t: Pt, r: number): Pt {
	const dx = t.x - v.x;
	const dy = t.y - v.y;
	const m = Math.hypot(dx, dy);
	if (m === 0) return { x: v.x, y: v.y };
	return { x: v.x + (dx / m) * r, y: v.y + (dy / m) * r };
}

// Build an SVG path through `pts`, rounding each interior vertex: go straight
// in to `r` before the corner, then a quadratic Bézier with the corner as its
// control point out to `r` past it. `r` is clamped to half of each adjoining
// segment so short segments can't overshoot or cross.
function roundedPolyline(pts: Pt[], r: number): string {
	if (pts.length < 2) return "";
	let d = `M ${pts[0].x},${pts[0].y}`;
	for (let i = 1; i < pts.length - 1; i++) {
		const p = pts[i - 1];
		const v = pts[i];
		const n = pts[i + 1];
		const len1 = Math.hypot(v.x - p.x, v.y - p.y);
		const len2 = Math.hypot(n.x - v.x, n.y - v.y);
		const rr = Math.min(r, len1 / 2, len2 / 2);
		if (rr === 0) {
			d += ` L ${v.x},${v.y}`;
			continue;
		}
		const a = trim(v, p, rr); // back off r along the incoming edge
		const b = trim(v, n, rr); // back off r along the outgoing edge
		d += ` L ${a.x},${a.y} Q ${v.x},${v.y} ${b.x},${b.y}`;
	}
	const last = pts[pts.length - 1];
	d += ` L ${last.x},${last.y}`;
	return d;
}

export function buildFilletPath(args: {
	sourceX: number;
	sourceY: number;
	targetX: number;
	targetY: number;
	sourcePosition: Position;
	targetPosition: Position;
}): string {
	const { sourceX, sourceY, targetX, targetY, sourcePosition, targetPosition } =
		args;
	const c1x = sourceX + stubDir(sourcePosition) * STUB;
	const c2x = targetX + stubDir(targetPosition) * STUB;
	const pts: Pt[] = [
		{ x: sourceX, y: sourceY }, // out of the source port
		{ x: c1x, y: sourceY }, // end of source stub (first corner)
		{ x: c2x, y: targetY }, // start of target stub (second corner)
		{ x: targetX, y: targetY }, // into the target port
	];
	return roundedPolyline(pts, RADIUS);
}

export function FilletEdge({
	id,
	sourceX,
	sourceY,
	targetX,
	targetY,
	sourcePosition,
	targetPosition,
	style,
	markerEnd,
	interactionWidth,
}: EdgeProps) {
	const path = buildFilletPath({
		sourceX,
		sourceY,
		targetX,
		targetY,
		sourcePosition,
		targetPosition,
	});
	return (
		<BaseEdge
			id={id}
			path={path}
			style={style}
			markerEnd={markerEnd}
			interactionWidth={interactionWidth}
		/>
	);
}

// The live wire drawn while dragging from a port — same fillet mechanic so it
// matches the committed edge. `toPosition` defaults to the opposite of the
// source side until the cursor is over a target.
export function FilletConnectionLine({
	fromX,
	fromY,
	toX,
	toY,
	fromPosition,
	toPosition,
	connectionLineStyle,
}: ConnectionLineComponentProps) {
	const path = buildFilletPath({
		sourceX: fromX,
		sourceY: fromY,
		targetX: toX,
		targetY: toY,
		sourcePosition: fromPosition,
		targetPosition: toPosition ?? Position.Left,
	});
	return (
		<path
			d={path}
			fill="none"
			className="react-flow__connection-path"
			style={connectionLineStyle}
		/>
	);
}
