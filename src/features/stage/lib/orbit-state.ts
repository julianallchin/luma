/**
 * Cursor-movement tracker for distinguishing "click" from "drag".
 *
 * Why this exists: R3F's built-in click-vs-drag check uses cursor delta
 * between its own pointerdown and pointerup events, but OrbitControls
 * captures the pointer and the intermediate pointermove events don't
 * reach R3F's tracker — so a left-button orbit drag arrives at R3F as a
 * pointerdown + pointerup on the same target with "zero" movement,
 * firing a synthetic `click` that selects whatever piece happened to be
 * under the cursor.
 *
 * We listen on the window at capture phase, so we see every pointer
 * event regardless of who calls setPointerCapture / stopPropagation.
 * Click handlers in the stage feature read {@link wasPointerDragged} and
 * skip selection when it returns true. The flag resets on the next tick
 * after pointerup so the synthetic click event still observes the drag.
 */

const DRAG_THRESHOLD_PX_SQ = 25; // 5px squared

let downPos: { x: number; y: number } | null = null;
let dragged = false;

function onDown(e: PointerEvent): void {
	downPos = { x: e.clientX, y: e.clientY };
	dragged = false;
}

function onMove(e: PointerEvent): void {
	if (!downPos) return;
	const dx = e.clientX - downPos.x;
	const dy = e.clientY - downPos.y;
	if (dx * dx + dy * dy > DRAG_THRESHOLD_PX_SQ) {
		dragged = true;
	}
}

function onUp(): void {
	// Defer the reset so the click event that fires right after
	// pointerup can still see the drag state.
	setTimeout(() => {
		downPos = null;
		dragged = false;
	}, 0);
}

/**
 * Install window-level pointer tracking. Returns a cleanup function.
 * Call once from the visualizer in a useEffect.
 */
export function installPointerDragTracker(): () => void {
	if (typeof window === "undefined") return () => {};
	window.addEventListener("pointerdown", onDown, true);
	window.addEventListener("pointermove", onMove, true);
	window.addEventListener("pointerup", onUp, true);
	return () => {
		window.removeEventListener("pointerdown", onDown, true);
		window.removeEventListener("pointermove", onMove, true);
		window.removeEventListener("pointerup", onUp, true);
	};
}

/**
 * Whether the pointer has moved past {@link DRAG_THRESHOLD_PX_SQ} since
 * the most recent pointerdown. Stays true for one tick after pointerup,
 * so the synthetic click can read it before reset.
 */
export function wasPointerDragged(): boolean {
	return dragged;
}
