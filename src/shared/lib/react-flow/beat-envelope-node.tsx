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

type ParamUpdates = Partial<Record<string, number>>;

interface EnvelopeCanvasProps {
	attack: number;
	decay: number;
	sustain: number;
	release: number;
	sustainLevel: number;
	attackCurve: number;
	decayCurve: number;
	onChange: (updates: ParamUpdates) => void;
}

function EnvelopeCanvas({
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

	const [attD, decD, susD, relD] = adsrDurations(
		attack,
		decay,
		sustain,
		release,
	);

	const xAttackEnd = attD;
	const xDecayEnd = attD + decD;
	const xSustainEnd = attD + decD + susD;

	const handles = React.useMemo(() => {
		const attackPt = {
			cx: toCanvasX(xAttackEnd),
			cy: toCanvasY(1),
			id: "attack" as HandleId,
		};
		const decayPt = {
			cx: toCanvasX(xDecayEnd),
			cy: toCanvasY(sustainLevel),
			id: "decay" as HandleId,
		};
		const sustainLevelPt = {
			cx: toCanvasX((xDecayEnd + xSustainEnd) / 2),
			cy: toCanvasY(sustainLevel),
			id: "sustain_level" as HandleId,
		};
		const sustainPt = {
			cx: toCanvasX(xSustainEnd),
			cy: toCanvasY(sustainLevel),
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
			sustainLevel,
			attackCurve,
			decayCurve,
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
			sustainLevel,
			attackCurve,
			decayCurve,
		);
		const decayCurvePt = {
			cx: toCanvasX(dCurveMidX),
			cy: toCanvasY(dCurveMidY),
			id: "decay_curve" as HandleId,
		};

		return {
			attack: attackPt,
			decay: decayPt,
			sustain_level: sustainLevelPt,
			sustain: sustainPt,
			attack_curve: attackCurvePt,
			decay_curve: decayCurvePt,
		};
	}, [
		xAttackEnd,
		xDecayEnd,
		xSustainEnd,
		sustainLevel,
		attD,
		decD,
		susD,
		relD,
		attackCurve,
		decayCurve,
	]);

	// Draw
	React.useEffect(() => {
		const canvas = canvasRef.current;
		if (!canvas) return;
		const ctx = canvas.getContext("2d");
		if (!ctx) return;

		const dpr = Math.max(window.devicePixelRatio ?? 1, 1);
		const sw = Math.round(W * dpr);
		const sh = Math.round(H * dpr);
		if (canvas.width !== sw || canvas.height !== sh) {
			canvas.width = sw;
			canvas.height = sh;
		}
		canvas.style.width = `${W}px`;
		canvas.style.height = `${H}px`;

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
		if (decD > 0.04)
			ctx.fillText("D", toCanvasX(xAttackEnd + decD / 2), labelY);
		if (susD > 0.04) ctx.fillText("S", toCanvasX(xDecayEnd + susD / 2), labelY);
		if (relD > 0.04)
			ctx.fillText("R", toCanvasX(xSustainEnd + relD / 2), labelY);

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
				sustainLevel,
				attackCurve,
				decayCurve,
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
				sustainLevel,
				attackCurve,
				decayCurve,
			);
			if (i === 0) ctx.moveTo(toCanvasX(t), toCanvasY(val));
			else ctx.lineTo(toCanvasX(t), toCanvasY(val));
		}
		ctx.strokeStyle = "rgba(96,165,250,0.9)";
		ctx.lineWidth = 2;
		ctx.lineJoin = "round";
		ctx.stroke();

		// Handles
		const active = dragging ?? hovered;
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
	}, [
		attD,
		decD,
		susD,
		relD,
		xAttackEnd,
		xDecayEnd,
		xSustainEnd,
		sustainLevel,
		attackCurve,
		decayCurve,
		handles,
		dragging,
		hovered,
	]);

	// Hit test
	const hitTest = React.useCallback(
		(px: number, py: number): HandleId | null => {
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
		[handles],
	);

	const getCanvasPos = React.useCallback((e: React.PointerEvent) => {
		const canvas = canvasRef.current;
		if (!canvas) return { x: 0, y: 0 };
		const rect = canvas.getBoundingClientRect();
		const scaleX = W / rect.width;
		const scaleY = H / rect.height;
		return {
			x: (e.clientX - rect.left) * scaleX,
			y: (e.clientY - rect.top) * scaleY,
		};
	}, []);

	// Keep latest props in refs so the document listener always reads fresh values
	const propsRef = React.useRef({
		attack,
		decay,
		sustain,
		release,
		sustainLevel,
		onChange,
	});
	propsRef.current = {
		attack,
		decay,
		sustain,
		release,
		sustainLevel,
		onChange,
	};

	// Document-level drag listeners
	React.useEffect(() => {
		if (!dragging) return;

		const canvas = canvasRef.current;
		if (!canvas) return;

		const onMove = (e: PointerEvent) => {
			e.preventDefault();
			e.stopPropagation();

			const rect = canvas.getBoundingClientRect();
			const scaleX = W / rect.width;
			const scaleY = H / rect.height;
			const px = (e.clientX - rect.left) * scaleX;
			const py = (e.clientY - rect.top) * scaleY;
			const normX = fromCanvasX(px);
			const normY = fromCanvasY(py);

			const {
				attack: a,
				decay: d,
				sustain: s,
				release: rel,
				sustainLevel: sl,
				onChange: emit,
			} = propsRef.current;
			const r = (v: number) =>
				Math.round(Math.max(0, Math.min(1, v)) * 100) / 100;
			const totalW = a + d + s + rel;
			const tgt = Math.max(0.01, Math.min(0.99, normX));

			switch (dragging) {
				case "attack": {
					// Move A|D boundary. Attack = tgt * totalW, decay absorbs rest.
					const newA = r(tgt * totalW);
					const newD = r(Math.max(0, a + d - newA));
					emit({ attack: newA, decay: newD });
					break;
				}
				case "decay": {
					// Move D|S boundary
					const newAD = tgt * totalW;
					const newD = r(Math.max(0, newAD - a));
					const newS = r(Math.max(0, a + d + s - newAD));
					emit({ decay: newD, sustain: newS });
					break;
				}
				case "sustain": {
					// Move S|R boundary
					const newADS = tgt * totalW;
					const newS = r(Math.max(0, newADS - a - d));
					const newR = r(Math.max(0.01, totalW - a - d - newS));
					emit({ sustain: newS, release: newR });
					break;
				}
				case "sustain_level": {
					emit({ sustain_level: r(normY) });
					break;
				}
				case "attack_curve": {
					const deviation = normY - 0.5;
					emit({
						attack_curve:
							Math.round(Math.max(-1, Math.min(1, -deviation * 2)) * 100) / 100,
					});
					break;
				}
				case "decay_curve": {
					const linearMidY = sl + (1 - sl) * 0.5;
					const deviation = normY - linearMidY;
					emit({
						decay_curve:
							Math.round(Math.max(-1, Math.min(1, -deviation * 2)) * 100) / 100,
					});
					break;
				}
			}
		};

		const onUp = () => {
			setDragging(null);
		};

		document.addEventListener("pointermove", onMove, { capture: true });
		document.addEventListener("pointerup", onUp, { capture: true });
		return () => {
			document.removeEventListener("pointermove", onMove, { capture: true });
			document.removeEventListener("pointerup", onUp, { capture: true });
		};
	}, [dragging]);

	const handlePointerDown = React.useCallback(
		(e: React.PointerEvent<HTMLCanvasElement>) => {
			const pos = getCanvasPos(e);
			const hit = hitTest(pos.x, pos.y);
			if (hit) {
				e.preventDefault();
				e.stopPropagation();
				setDragging(hit);
			}
		},
		[hitTest, getCanvasPos],
	);

	const handleDoubleClick = React.useCallback(
		(e: React.MouseEvent<HTMLCanvasElement>) => {
			const canvas = canvasRef.current;
			if (!canvas) return;
			const rect = canvas.getBoundingClientRect();
			const scaleX = W / rect.width;
			const scaleY = H / rect.height;
			const px = (e.clientX - rect.left) * scaleX;
			const py = (e.clientY - rect.top) * scaleY;
			const hit = hitTest(px, py);
			if (hit === "attack_curve" || hit === "decay_curve") {
				e.preventDefault();
				e.stopPropagation();
				onChange({ [hit]: 0 });
			}
		},
		[hitTest, onChange],
	);

	const handlePointerMove = React.useCallback(
		(e: React.PointerEvent<HTMLCanvasElement>) => {
			if (dragging) return;
			const pos = getCanvasPos(e);
			setHovered(hitTest(pos.x, pos.y));
		},
		[dragging, getCanvasPos, hitTest],
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

			<div className="h-px bg-border -mx-2" />

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
