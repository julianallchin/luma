import * as React from "react";
import { type NodeProps, useEdges } from "reactflow";
import { useGraphStore } from "@/features/patterns/stores/use-graph-store";
import { Checkbox } from "@/shared/components/ui/checkbox";
import { Label } from "@/shared/components/ui/label";
import { Slider } from "@/shared/components/ui/slider";
import { cn } from "@/shared/lib/utils";
import { BaseNode } from "./base-node";
import type { BaseNodeData } from "./types";

const SUBDIVISIONS = [0.25, 0.5, 1, 2, 4];
const SUBDIVISION_LABELS: Record<number, string> = {
	0.25: "1/4",
	0.5: "1/2",
	1: "1",
	2: "2",
	4: "4",
};

// --- Envelope math (mirrors Rust calc_envelope / shape_curve) ---

function shapeCurve(x: number, curve: number): number {
	const cx = Math.max(0, Math.min(1, x));
	if (Math.abs(curve) < 0.001) return cx;
	if (curve > 0) {
		const p = 1 + curve * 5;
		return cx ** p;
	}
	const p = 1 + -curve * 5;
	return 1 - (1 - cx) ** p;
}

function adsrDurations(
	attack: number,
	decay: number,
	sustain: number,
	release: number,
): [number, number, number, number] {
	const aw = Math.max(0, Math.min(1, attack));
	const dw = Math.max(0, Math.min(1, decay));
	const sw = Math.max(0, Math.min(1, sustain));
	const rw = Math.max(0, Math.min(1, release));
	const sum = aw + dw + sw + rw;
	if (sum < 1e-6) return [0, 0, 0, 0];
	const scale = 1 / sum;
	return [aw * scale, dw * scale, sw * scale, rw * scale];
}

function calcEnvelope(
	t: number,
	peak: number,
	att: number,
	dec: number,
	sus: number,
	rel: number,
	sustainLevel: number,
	aCurve: number,
	dCurve: number,
): number {
	if (t < peak - att) return 0;
	if (t <= peak) {
		if (att <= 0) return 1;
		const x = (t - (peak - att)) / att;
		return shapeCurve(x, aCurve);
	}
	const decEnd = peak + dec;
	if (t <= decEnd) {
		if (dec <= 0) return sustainLevel;
		const x = (t - peak) / dec;
		return sustainLevel + (1 - sustainLevel) * shapeCurve(1 - x, dCurve);
	}
	const susEnd = decEnd + sus;
	if (t <= susEnd) return sustainLevel;
	const relEnd = susEnd + rel;
	if (t <= relEnd) {
		if (rel <= 0) return 0;
		const x = (t - susEnd) / rel;
		return sustainLevel * (1 - x);
	}
	return 0;
}

// --- Canvas dimensions ---
const W = 280;
const H = 140;
const PAD_X = 12;
const PAD_Y = 14;
const DRAW_W = W - PAD_X * 2;
const DRAW_H = H - PAD_Y * 2;

function toCanvasX(normX: number) {
	return PAD_X + normX * DRAW_W;
}
function toCanvasY(normY: number) {
	return PAD_Y + (1 - normY) * DRAW_H;
}
function fromCanvasX(px: number) {
	return Math.max(0, Math.min(1, (px - PAD_X) / DRAW_W));
}
function fromCanvasY(py: number) {
	return Math.max(0, Math.min(1, 1 - (py - PAD_Y) / DRAW_H));
}

type HandleId =
	| "attack"
	| "decay"
	| "sustain"
	| "sustain_level"
	| "attack_curve"
	| "decay_curve";

export type ParamUpdates = Partial<Record<string, number>>;

type EnvelopeVals = {
	attack: number;
	decay: number;
	sustain: number;
	release: number;
	sustainLevel: number;
	attackCurve: number;
	decayCurve: number;
};

function computeHandles(vals: EnvelopeVals) {
	const [attD, decD, susD, relD] = adsrDurations(
		vals.attack,
		vals.decay,
		vals.sustain,
		vals.release,
	);
	const xAttackEnd = attD;
	const xDecayEnd = attD + decD;
	const xSustainEnd = attD + decD + susD;

	const attackPt = {
		cx: toCanvasX(xAttackEnd),
		cy: toCanvasY(1),
		id: "attack" as HandleId,
	};
	const decayPt = {
		cx: toCanvasX(xDecayEnd),
		cy: toCanvasY(vals.sustainLevel),
		id: "decay" as HandleId,
	};
	const sustainLevelPt = {
		cx: toCanvasX((xDecayEnd + xSustainEnd) / 2),
		cy: toCanvasY(vals.sustainLevel),
		id: "sustain_level" as HandleId,
	};
	const sustainPt = {
		cx: toCanvasX(xSustainEnd),
		cy: toCanvasY(vals.sustainLevel),
		id: "sustain" as HandleId,
	};

	const aCurveMidX = xAttackEnd / 2;
	const aCurveMidY = calcEnvelope(
		aCurveMidX,
		xAttackEnd,
		attD,
		decD,
		susD,
		relD,
		vals.sustainLevel,
		vals.attackCurve,
		vals.decayCurve,
	);
	const attackCurvePt = {
		cx: toCanvasX(aCurveMidX),
		cy: toCanvasY(aCurveMidY),
		id: "attack_curve" as HandleId,
	};

	const dCurveMidX = xAttackEnd + decD / 2;
	const dCurveMidY = calcEnvelope(
		dCurveMidX,
		xAttackEnd,
		attD,
		decD,
		susD,
		relD,
		vals.sustainLevel,
		vals.attackCurve,
		vals.decayCurve,
	);
	const decayCurvePt = {
		cx: toCanvasX(dCurveMidX),
		cy: toCanvasY(dCurveMidY),
		id: "decay_curve" as HandleId,
	};

	return {
		handles: {
			attack: attackPt,
			decay: decayPt,
			sustain_level: sustainLevelPt,
			sustain: sustainPt,
			attack_curve: attackCurvePt,
			decay_curve: decayCurvePt,
		},
		attD,
		decD,
		susD,
		relD,
		xAttackEnd,
		xDecayEnd,
		xSustainEnd,
	};
}

/**
 * Imperative draw — called directly from pointermove during a handle drag so
 * the envelope tracks the cursor independently of React re-renders and the
 * (throttled) store/graph update cycle.
 */
function drawEnvelope(
	canvas: HTMLCanvasElement,
	vals: EnvelopeVals,
	active: HandleId | null,
) {
	const ctx = canvas.getContext("2d");
	if (!ctx) return;

	const {
		handles,
		attD,
		decD,
		susD,
		relD,
		xAttackEnd,
		xDecayEnd,
		xSustainEnd,
	} = computeHandles(vals);

	// CSS size comes from the JSX style prop — don't touch canvas.style here:
	// this runs per pointermove during drags, and style writes dirty layout.
	const dpr = Math.max(window.devicePixelRatio ?? 1, 1);
	const sw = Math.round(W * dpr);
	const sh = Math.round(H * dpr);
	if (canvas.width !== sw || canvas.height !== sh) {
		canvas.width = sw;
		canvas.height = sh;
	}

	ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
	ctx.clearRect(0, 0, W, H);

	// Background grid
	ctx.strokeStyle = "rgba(255,255,255,0.04)";
	ctx.lineWidth = 1;
	for (let i = 0; i <= 4; i++) {
		const y = toCanvasY(i / 4);
		ctx.beginPath();
		ctx.moveTo(PAD_X, y);
		ctx.lineTo(W - PAD_X, y);
		ctx.stroke();
	}

	// Phase boundary lines
	ctx.strokeStyle = "rgba(255,255,255,0.08)";
	ctx.setLineDash([3, 3]);
	for (const x of [xAttackEnd, xDecayEnd, xSustainEnd]) {
		const px = toCanvasX(x);
		ctx.beginPath();
		ctx.moveTo(px, PAD_Y);
		ctx.lineTo(px, H - PAD_Y);
		ctx.stroke();
	}
	ctx.setLineDash([]);

	// Phase labels
	ctx.font = "9px ui-monospace, SFMono-Regular, Menlo, monospace";
	ctx.fillStyle = "rgba(255,255,255,0.25)";
	ctx.textAlign = "center";
	ctx.textBaseline = "bottom";
	const labelY = H - 2;
	if (attD > 0.04) ctx.fillText("A", toCanvasX(xAttackEnd / 2), labelY);
	if (decD > 0.04) ctx.fillText("D", toCanvasX(xAttackEnd + decD / 2), labelY);
	if (susD > 0.04) ctx.fillText("S", toCanvasX(xDecayEnd + susD / 2), labelY);
	if (relD > 0.04) ctx.fillText("R", toCanvasX(xSustainEnd + relD / 2), labelY);

	// Envelope curve
	const SAMPLES = 200;
	ctx.beginPath();
	ctx.moveTo(toCanvasX(0), toCanvasY(0));
	for (let i = 0; i <= SAMPLES; i++) {
		const t = i / SAMPLES;
		const val = calcEnvelope(
			t,
			xAttackEnd,
			attD,
			decD,
			susD,
			relD,
			vals.sustainLevel,
			vals.attackCurve,
			vals.decayCurve,
		);
		ctx.lineTo(toCanvasX(t), toCanvasY(val));
	}
	ctx.lineTo(toCanvasX(1), toCanvasY(0));
	ctx.closePath();

	const grad = ctx.createLinearGradient(0, PAD_Y, 0, H - PAD_Y);
	grad.addColorStop(0, "rgba(59,130,246,0.25)");
	grad.addColorStop(1, "rgba(59,130,246,0.03)");
	ctx.fillStyle = grad;
	ctx.fill();

	ctx.beginPath();
	for (let i = 0; i <= SAMPLES; i++) {
		const t = i / SAMPLES;
		const val = calcEnvelope(
			t,
			xAttackEnd,
			attD,
			decD,
			susD,
			relD,
			vals.sustainLevel,
			vals.attackCurve,
			vals.decayCurve,
		);
		if (i === 0) ctx.moveTo(toCanvasX(t), toCanvasY(val));
		else ctx.lineTo(toCanvasX(t), toCanvasY(val));
	}
	ctx.strokeStyle = "rgba(96,165,250,0.9)";
	ctx.lineWidth = 2;
	ctx.lineJoin = "round";
	ctx.stroke();

	// Handles
	for (const h of Object.values(handles)) {
		const isActive = h.id === active;
		const isCurve = h.id === "attack_curve" || h.id === "decay_curve";
		const radius = isCurve ? 4 : 5;

		ctx.beginPath();
		if (isCurve) {
			ctx.moveTo(h.cx, h.cy - radius);
			ctx.lineTo(h.cx + radius, h.cy);
			ctx.lineTo(h.cx, h.cy + radius);
			ctx.lineTo(h.cx - radius, h.cy);
			ctx.closePath();
		} else {
			ctx.arc(h.cx, h.cy, radius, 0, Math.PI * 2);
		}

		ctx.fillStyle = isActive
			? "rgba(96,165,250,1)"
			: isCurve
				? "rgba(168,85,247,0.85)"
				: "rgba(96,165,250,0.7)";
		ctx.fill();
		ctx.strokeStyle = isActive ? "white" : "rgba(255,255,255,0.5)";
		ctx.lineWidth = isActive ? 2 : 1;
		ctx.stroke();
	}
}

export interface EnvelopeCanvasProps {
	attack: number;
	decay: number;
	sustain: number;
	release: number;
	sustainLevel: number;
	attackCurve: number;
	decayCurve: number;
	onChange: (updates: ParamUpdates) => void;
}

export function EnvelopeCanvas({
	attack,
	decay,
	sustain,
	release,
	sustainLevel,
	attackCurve,
	decayCurve,
	onChange,
}: EnvelopeCanvasProps) {
	const canvasRef = React.useRef<HTMLCanvasElement>(null);
	const [dragging, setDragging] = React.useState<HandleId | null>(null);
	const [hovered, setHovered] = React.useState<HandleId | null>(null);

	// Live values: during a drag, the pointermove handler updates these and
	// draws directly — React/store only sees throttled emissions. When idle
	// they mirror props.
	const liveValsRef = React.useRef<EnvelopeVals>({
		attack,
		decay,
		sustain,
		release,
		sustainLevel,
		attackCurve,
		decayCurve,
	});
	const onChangeRef = React.useRef(onChange);
	onChangeRef.current = onChange;

	// Idle-path draw (prop changes, hover state). During a drag the imperative
	// draws in the move handler are authoritative — use the live values so a
	// re-render from a throttled emit can't repaint stale ones.
	React.useEffect(() => {
		const canvas = canvasRef.current;
		if (!canvas) return;
		const propVals = {
			attack,
			decay,
			sustain,
			release,
			sustainLevel,
			attackCurve,
			decayCurve,
		};
		if (!dragging) liveValsRef.current = propVals;
		drawEnvelope(
			canvas,
			dragging ? liveValsRef.current : propVals,
			dragging ?? hovered,
		);
	}, [
		attack,
		decay,
		sustain,
		release,
		sustainLevel,
		attackCurve,
		decayCurve,
		dragging,
		hovered,
	]);

	// Hit test against the live geometry
	const hitTest = React.useCallback(
		(px: number, py: number): HandleId | null => {
			const { handles } = computeHandles(liveValsRef.current);
			let closest: HandleId | null = null;
			let closestDist = 14;
			for (const h of Object.values(handles)) {
				const d = Math.hypot(px - h.cx, py - h.cy);
				if (d < closestDist) {
					closestDist = d;
					closest = h.id;
				}
			}
			return closest;
		},
		[],
	);

	// getBoundingClientRect forces a synchronous layout pass — never call it
	// per pointermove (the editor DOM is mutating ~20×/s during live runs, so
	// each call is a full relayout: classic thrashing). Cache the rect on
	// pointerenter/pointerdown instead; zoom/pan can't change mid-interaction.
	const rectRef = React.useRef<DOMRect | null>(null);
	const refreshRect = React.useCallback(() => {
		const canvas = canvasRef.current;
		if (canvas) rectRef.current = canvas.getBoundingClientRect();
		return rectRef.current;
	}, []);

	const toLocal = React.useCallback(
		(clientX: number, clientY: number) => {
			const rect = rectRef.current ?? refreshRect();
			if (!rect) return { x: 0, y: 0 };
			const scaleX = W / rect.width;
			const scaleY = H / rect.height;
			return {
				x: (clientX - rect.left) * scaleX,
				y: (clientY - rect.top) * scaleY,
			};
		},
		[refreshRect],
	);

	// Throttled emission to the store: the canvas already tracks the pointer
	// imperatively, so the store (→ node re-render → graph re-execution) only
	// needs updates at the graph-run cadence. Trailing flush keeps the final
	// value; pointerup flushes immediately.
	const EMIT_THROTTLE_MS = 50;
	const pendingEmitRef = React.useRef<ParamUpdates | null>(null);
	const emitTimeoutRef = React.useRef<NodeJS.Timeout | null>(null);
	const lastEmitRef = React.useRef(Number.NEGATIVE_INFINITY);

	// Document-level drag listeners
	React.useEffect(() => {
		if (!dragging) return;

		const canvas = canvasRef.current;
		if (!canvas) return;

		const flushEmit = () => {
			const updates = pendingEmitRef.current;
			pendingEmitRef.current = null;
			if (updates) {
				lastEmitRef.current = performance.now();
				onChangeRef.current(updates);
			}
		};
		const queueEmit = (updates: ParamUpdates) => {
			pendingEmitRef.current = { ...pendingEmitRef.current, ...updates };
			const elapsed = performance.now() - lastEmitRef.current;
			if (elapsed >= EMIT_THROTTLE_MS) {
				flushEmit();
			} else if (!emitTimeoutRef.current) {
				emitTimeoutRef.current = setTimeout(() => {
					emitTimeoutRef.current = null;
					flushEmit();
				}, EMIT_THROTTLE_MS - elapsed);
			}
		};

		// Apply updates locally + draw now; emit to the store on the throttle.
		const apply = (updates: ParamUpdates) => {
			const v = { ...liveValsRef.current };
			if (updates.attack !== undefined) v.attack = updates.attack;
			if (updates.decay !== undefined) v.decay = updates.decay;
			if (updates.sustain !== undefined) v.sustain = updates.sustain;
			if (updates.release !== undefined) v.release = updates.release;
			if (updates.sustain_level !== undefined)
				v.sustainLevel = updates.sustain_level;
			if (updates.attack_curve !== undefined)
				v.attackCurve = updates.attack_curve;
			if (updates.decay_curve !== undefined) v.decayCurve = updates.decay_curve;
			liveValsRef.current = v;
			drawEnvelope(canvas, v, dragging);
			queueEmit(updates);
		};

		const onMove = (e: PointerEvent) => {
			e.preventDefault();
			e.stopPropagation();

			const { x: px, y: py } = toLocal(e.clientX, e.clientY);
			const normX = fromCanvasX(px);
			const normY = fromCanvasY(py);

			const {
				attack: a,
				decay: d,
				sustain: s,
				release: rel,
				sustainLevel: sl,
			} = liveValsRef.current;
			const r = (v: number) =>
				Math.round(Math.max(0, Math.min(1, v)) * 100) / 100;
			const totalW = a + d + s + rel;
			const tgt = Math.max(0, Math.min(1, normX));

			// The four params are unnormalized weights; the envelope only
			// depends on their ratios (display + Rust both renormalize). We
			// drag boundaries in normalized space [0,1] and emit weights that
			// already sum to 1 — this keeps every weight in r()'s [0,1] range
			// (so a boundary can reach its neighbor and fully close a phase)
			// and self-heals any node whose weights drifted off 1.
			const an = a / totalW;
			const dn = d / totalW;
			const sn = s / totalW;
			const rn = rel / totalW;
			const bAD = an; // A|D boundary
			const bDS = an + dn; // D|S boundary
			const bSR = an + dn + sn; // S|R boundary

			switch (dragging) {
				case "attack": {
					// Move A|D boundary within [0, D|S] — trades only with D.
					const x = Math.min(Math.max(tgt, 0), bDS);
					apply({
						attack: r(x),
						decay: r(bDS - x),
						sustain: r(sn),
						release: r(rn),
					});
					break;
				}
				case "decay": {
					// Move D|S boundary within [A|D, S|R] — trades only with S.
					const x = Math.min(Math.max(tgt, bAD), bSR);
					apply({
						attack: r(an),
						decay: r(x - bAD),
						sustain: r(bSR - x),
						release: r(rn),
					});
					break;
				}
				case "sustain": {
					// Move S|R boundary within [D|S, 1] — trades only with R.
					const x = Math.min(Math.max(tgt, bDS), 1);
					apply({
						attack: r(an),
						decay: r(dn),
						sustain: r(x - bDS),
						release: r(1 - x),
					});
					break;
				}
				case "sustain_level": {
					apply({ sustain_level: r(normY) });
					break;
				}
				case "attack_curve": {
					const deviation = normY - 0.5;
					apply({
						attack_curve:
							Math.round(Math.max(-1, Math.min(1, -deviation * 2)) * 100) / 100,
					});
					break;
				}
				case "decay_curve": {
					const linearMidY = sl + (1 - sl) * 0.5;
					const deviation = normY - linearMidY;
					apply({
						decay_curve:
							Math.round(Math.max(-1, Math.min(1, -deviation * 2)) * 100) / 100,
					});
					break;
				}
			}
		};

		const onUp = () => {
			if (emitTimeoutRef.current) {
				clearTimeout(emitTimeoutRef.current);
				emitTimeoutRef.current = null;
			}
			flushEmit();
			setDragging(null);
		};

		document.addEventListener("pointermove", onMove, { capture: true });
		document.addEventListener("pointerup", onUp, { capture: true });
		return () => {
			document.removeEventListener("pointermove", onMove, { capture: true });
			document.removeEventListener("pointerup", onUp, { capture: true });
			if (emitTimeoutRef.current) {
				clearTimeout(emitTimeoutRef.current);
				emitTimeoutRef.current = null;
			}
			flushEmit();
		};
	}, [dragging]);

	const handlePointerDown = React.useCallback(
		(e: React.PointerEvent<HTMLCanvasElement>) => {
			refreshRect();
			const pos = toLocal(e.clientX, e.clientY);
			const hit = hitTest(pos.x, pos.y);
			if (hit) {
				e.preventDefault();
				e.stopPropagation();
				setDragging(hit);
			}
		},
		[hitTest, toLocal, refreshRect],
	);

	const handleDoubleClick = React.useCallback(
		(e: React.MouseEvent<HTMLCanvasElement>) => {
			refreshRect();
			const pos = toLocal(e.clientX, e.clientY);
			const hit = hitTest(pos.x, pos.y);
			if (hit === "attack_curve" || hit === "decay_curve") {
				e.preventDefault();
				e.stopPropagation();
				onChange({ [hit]: 0 });
			}
		},
		[hitTest, toLocal, refreshRect, onChange],
	);

	// Hover hit-testing only re-renders when the hovered handle actually
	// changes; the rect cached on pointerenter keeps this layout-free.
	const handlePointerMove = React.useCallback(
		(e: React.PointerEvent<HTMLCanvasElement>) => {
			if (dragging) return;
			const pos = toLocal(e.clientX, e.clientY);
			setHovered(hitTest(pos.x, pos.y));
		},
		[dragging, toLocal, hitTest],
	);

	return (
		<canvas
			ref={canvasRef}
			width={W}
			height={H}
			className="block cursor-default nodrag"
			style={{
				width: `${W}px`,
				height: `${H}px`,
				touchAction: "none",
				cursor: dragging ? "grabbing" : hovered ? "grab" : "default",
			}}
			onPointerEnter={refreshRect}
			onPointerDown={handlePointerDown}
			onPointerMove={handlePointerMove}
			onDoubleClick={handleDoubleClick}
			onPointerLeave={() => {
				if (!dragging) setHovered(null);
			}}
			role="img"
			aria-label="ADSR envelope editor"
		/>
	);
}

export function BeatEnvelopeNode(props: NodeProps<BaseNodeData>) {
	const { data, id } = props;
	const edges = useEdges();
	const params = useGraphStore(
		(state) => state.nodeParams[id] ?? ({} as Record<string, unknown>),
	);
	const setParam = useGraphStore((state) => state.setParam);

	const hasSubdivisionInput = edges.some(
		(edge) => edge.target === id && edge.targetHandle === "subdivision",
	);
	const hasOffsetInput = edges.some(
		(edge) => edge.target === id && edge.targetHandle === "offset",
	);

	const getNum = (key: string, def: number) => (params[key] as number) ?? def;
	const getBool = (key: string, def: boolean) =>
		(params[key] as number) === 1.0 || ((params[key] as boolean) ?? def);

	const updateNum = (key: string, val: number) => setParam(id, key, val);
	const updateBool = (key: string, val: boolean) =>
		setParam(id, key, val ? 1.0 : 0.0);

	// Batch-update multiple params atomically via Zustand setState
	const handleEnvelopeChange = React.useCallback(
		(updates: ParamUpdates) => {
			useGraphStore.setState((state) => {
				const existing = state.nodeParams[id] ?? {};
				return {
					nodeParams: {
						...state.nodeParams,
						[id]: { ...existing, ...updates },
					},
					version: state.version + 1,
				};
			});
		},
		[id],
	);

	const paramControls = (
		<div
			className="flex flex-col gap-1 p-1"
			onPointerDown={(e) => e.stopPropagation()}
		>
			<EnvelopeCanvas
				attack={getNum("attack", 0.3)}
				decay={getNum("decay", 0.2)}
				sustain={getNum("sustain", 0.3)}
				release={getNum("release", 0.2)}
				sustainLevel={getNum("sustain_level", 0.7)}
				attackCurve={getNum("attack_curve", 0.0)}
				decayCurve={getNum("decay_curve", 0.0)}
				onChange={handleEnvelopeChange}
			/>

			<div className="h-0.5 bg-gutter -mx-2" />

			{!hasSubdivisionInput && (
				<div className="space-y-1">
					<Label className="text-[10px] text-muted-foreground">
						Subdivision
					</Label>
					<div className="flex bg-input border p-0.5">
						{SUBDIVISIONS.map((sub) => {
							const current = getNum("subdivision", 1.0);
							const isActive = Math.abs(current - sub) < 0.01;
							return (
								<button
									key={sub}
									type="button"
									onClick={() => updateNum("subdivision", sub)}
									className={cn(
										"flex-1 px-1 text-xs font-medium transition-all",
										isActive
											? "bg-muted text-foreground"
											: "text-muted-foreground hover:text-foreground hover:bg-card",
									)}
								>
									{SUBDIVISION_LABELS[sub]}
								</button>
							);
						})}
					</div>
				</div>
			)}

			<div className="flex items-center gap-2">
				<Checkbox
					id={`${id}-only_downbeats`}
					checked={getBool("only_downbeats", false)}
					onCheckedChange={(c) => updateBool("only_downbeats", c === true)}
				/>
				<Label
					htmlFor={`${id}-only_downbeats`}
					className="text-xs cursor-pointer select-none"
				>
					Only Downbeats
				</Label>
			</div>

			<div className="flex items-center gap-2">
				<Checkbox
					id={`${id}-anticipate`}
					checked={getBool("anticipate", false)}
					onCheckedChange={(c) => updateBool("anticipate", c === true)}
				/>
				<Label
					htmlFor={`${id}-anticipate`}
					className="text-xs cursor-pointer select-none"
					title="Start the attack before each beat so the peak lands on the beat. Off: the attack starts at the beat and ramps up after it."
				>
					Anticipate
				</Label>
			</div>

			<div className="grid grid-cols-2 gap-2">
				<div className="space-y-0.5">
					<Label className="text-[10px] text-muted-foreground">Amplitude</Label>
					<div className="nodrag">
						<Slider
							id={`${id}-amplitude`}
							min={0}
							max={2}
							step={0.01}
							value={getNum("amplitude", 1.0)}
							onChange={(e) => updateNum("amplitude", Number(e.target.value))}
							className="flex-1 h-4"
						/>
					</div>
				</div>
				{!hasOffsetInput && (
					<div className="space-y-0.5">
						<Label className="text-[10px] text-muted-foreground">Offset</Label>
						<div className="nodrag">
							<Slider
								id={`${id}-offset`}
								min={-1}
								max={1}
								step={0.01}
								value={getNum("offset", 0.0)}
								onChange={(e) => updateNum("offset", Number(e.target.value))}
								className="flex-1 h-4"
							/>
						</div>
					</div>
				)}
			</div>
		</div>
	);

	return <BaseNode {...props} data={{ ...data, paramControls }} />;
}
