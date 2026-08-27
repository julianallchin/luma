/**
 * The surface `exec(code)` runs against.
 *
 * `globalThis` persists between `exec` calls, so anything you assign is still
 * there next time. `reset()` throws that away and rebuilds the app.
 *
 * The one rule that is not obvious: node ids are scoped to the frame they were
 * snapshotted in. Every mutating call carries that frame back, and acting on a
 * frame that is no longer current is a hard error rather than a click at stale
 * coordinates. Snapshot, act, snapshot again.
 */

/** A rectangle in window space, in logical pixels. */
interface Bounds {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** What kind of control a node is. */
type Role =
  | "button"
  | "toggle"
  | "checkbox"
  | "input"
  | "select"
  | "slider"
  | "row"
  | "card"
  | "text"
  /**
   * A labelled state pill — a tool call in the agent chat. Its own role
   * because it is none of the others: not a button (not pressable at rest),
   * not text (it carries state), not a row.
   */
  | "chip";

/** One control, as it existed in one frame. */
interface Node {
  /** The frame this node was snapshotted in. */
  frame: number;
  /** Index within that frame. Meaningless in any other frame. */
  id: number;
  role: Role;
  label: string;
  /**
   * Where it is, clipped to whatever is masking it. A control masked out of
   * view collapses to zero along whichever axis it left — off to the right
   * gives zero width, scrolled past the bottom gives zero height, so test
   * `width && height` rather than expecting `0 × 0`. Either way there is no
   * point on screen that would hit it, and acting on it is an error.
   */
  bounds: Bounds;
  /** Whether it would accept input. Independent of whether it is on screen. */
  enabled: boolean;
  /** Where `app.key()` and `app.action()` will land. */
  focused: boolean;
}

/** Match by exact role and/or label. Omitted fields are wildcards. */
interface NodeQuery {
  role?: Role;
  label?: string;
}

interface Snapshot {
  frame: number;
  nodes: Node[];
  /**
   * First node matching a predicate or a `{role, label}` query, or
   * `undefined`. Filtering is deliberately yours: `nodes` is a plain array,
   * so `.filter`, `.map` and `.find` all work as usual.
   */
  find(query: NodeQuery | ((node: Node) => boolean)): Node | undefined;
  /** Every node matching, in frame order. */
  findAll(query: NodeQuery | ((node: Node) => boolean)): Node[];
}

/**
 * Modifier keys held for a gesture. `"secondary"` is the platform key —
 * command on macOS, control elsewhere — under the name gpui gives it, and is
 * what a handler branching on cmd-or-ctrl reads.
 */
type Modifier =
  | "control"
  | "alt"
  | "shift"
  | "platform"
  | "secondary"
  | "function";

/** Which physical button a pointer gesture presses. Defaults to `"left"`. */
type MouseButton = "left" | "right" | "middle";

/**
 * What every acting call accepts.
 *
 * `restale` says what to do when the node you are acting on came from an
 * older frame. It defaults to erroring; `"match"` re-snapshots and finds the
 * node again by `(role, label)` — use it only where you know a redraw is
 * expected and harmless.
 *
 * `modifiers` are held down for the whole gesture, every intermediate pointer
 * move included. That is not decoration: shift-click to extend a selection
 * and alt-drag to duplicate are read off the event, and a gesture that let go
 * of the key half way is a different gesture.
 */
interface ActOptions {
  restale?: "error" | "match";
  modifiers?: Modifier[];
}

/** What one settled frame cost the pump, in milliseconds. */
interface FrameTiming {
  /** The frame this was measured around — matches `Snapshot.frame`. */
  frame: number;
  /**
   * Draining the app's own work before the draw: event handlers, spawned
   * tasks, anything a pointer step kicked off.
   */
  parkedMs: number;
  /** Walking the element tree into a scene — layout, prepaint, paint. */
  drawMs: number;
}

interface Timings {
  /**
   * Which mode measured these. Both modes time the same two CPU phases;
   * neither times the GPU (see `app.timings`), so pixel differs only in
   * feeding layout real glyph metrics.
   */
  mode: "headless" | "pixel";
  /** Oldest first, capped at the last 512 frames. */
  frames: FrameTiming[];
}

interface App {
  /** Settle the app, draw a frame, and describe every control in it. */
  snapshot(): Snapshot;

  /**
   * Press and release at the centre of `node`.
   *
   * `button: "right"` is how a context menu is opened — gpui delivers the
   * button on the event and a handler that filters to left will never see it.
   * `count: 2` is a double-click, delivered the way the platform delivers
   * one: a whole single click, then a second press carrying `click_count: 2`.
   * Each press-release gets its own settled frame, so a handler that reacts to
   * the first has drawn before the second arrives.
   */
  click(
    node: Node,
    options?: ActOptions & { button?: MouseButton; count?: number },
  ): { frame: number };

  /**
   * Press at `from`, walk the pointer to `to` over `steps` moves, release.
   * The app gets a settled frame between every step, because drag previews
   * only appear on a repaint.
   *
   * `from` is a node, or `{x, y}` — a point in the window, for gestures that
   * start where no control is: panning a canvas, sweeping a marquee across
   * empty ground. `to` is either another node, or `{dx, dy}` — a displacement
   * in logical pixels from where the drag started. Use the delta form for
   * anything on a canvas, where the destination is a position and not a
   * control. A delta that would end outside the window is an error, not a
   * shorter drag.
   */
  drag(
    from: Node | { x: number; y: number },
    to: Node | { dx: number; dy: number },
    options?: ActOptions & { steps?: number; button?: MouseButton },
  ): { frame: number };

  /** Click `node` to focus it, then type `text` into it. */
  type(node: Node, text: string, options?: ActOptions): { frame: number };

  /** A space-separated keystroke sequence, e.g. `"cmd-p escape"`. */
  key(keys: string): { frame: number };

  /**
   * Dispatch a registered gpui action by name to the focused node. Prefer
   * this over a click wherever an action exists: it does not depend on where
   * anything is on screen.
   */
  action(name: string, payload?: unknown): { frame: number };

  /**
   * Let time pass: `n` rounds of "wait `waitMs`, then settle and draw".
   *
   * The wait is the point. `snapshot` and the acting calls drain gpui's own
   * executors before they return, but Luma's data lives behind a separate
   * Tokio runtime, and gpui has no way to know that a query is still in
   * flight. `app.frames(3)` is how you wait for a screen to fill in.
   */
  frames(n?: number, options?: { waitMs?: number }): { frame: number };

  /**
   * Turn the wheel over a control, or over a point in the window.
   *
   * `dx`/`dy` are pixels and are split evenly across `steps`, each step
   * getting its own settled frame — the same walk `drag` does, and for the
   * same reason: a handler that accumulates (an exponential zoom, say) lands
   * somewhere different for one flick than for the same distance in ten, and
   * ten is what a real wheel sends.
   *
   * The pointer is moved to `where` first, because on a device it is already
   * there and gpui routes a wheel by where the pointer last *was*. So a
   * scroll lands on the surface it names whether or not anything hovered it,
   * and it leaves the pointer there afterwards.
   *
   * This is the only way to reach a `ScrollWheelEvent` handler. Anything a
   * canvas puts behind the wheel — the track editor's zoom, the graph
   * editor's pan — is unreachable by clicking, so reach for this rather than
   * approximating a wheel with a drag.
   */
  scroll(
    where: Node | { x: number; y: number },
    options?: ActOptions & {
      dx?: number;
      dy?: number;
      steps?: number;
    },
  ): { frame: number };

  /**
   * Every frame the pump has timed, oldest first. Reading does not clear and
   * does not settle, so it costs nothing and two readers cannot interfere —
   * filter by `frame` against what a command returned.
   *
   * Every command that settles is timed, not just `frames`: a scrub is a
   * `drag`, and the frames worth measuring are the ones it draws between
   * pointer steps. So the shape of a measurement is
   * `app.drag(node, {dx: 400, dy: 0}, {steps: 40})` and then reading the
   * frames at or after the frame that returned.
   *
   * **These are the CPU half of a frame, not a frame time.** `drawMs` is the
   * scene build; handing that scene to the renderer is not measured in either
   * mode, because the only public entry point for it is gated behind gpui's
   * `bench` feature at the pinned revision.
   */
  timings(): Timings;

  /**
   * Render to a PNG on disk and return where it went. Pixel mode only —
   * throws in headless mode, which has no renderer.
   */
  screenshot(options?: {
    node?: Node;
  }): { path: string; width: number; height: number };

  /** This file, verbatim. */
  help(): string;
}

declare const app: App;
