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
  | "text";

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
 * What to do when the node you are acting on came from an older frame.
 * Defaults to erroring. `"match"` re-snapshots and finds the node again by
 * `(role, label)` — use it only where you know a redraw is expected and
 * harmless.
 */
interface ActOptions {
  restale?: "error" | "match";
}

interface App {
  /** Settle the app, draw a frame, and describe every control in it. */
  snapshot(): Snapshot;

  /** Press and release at the centre of `node`. */
  click(node: Node, options?: ActOptions): { frame: number };

  /**
   * Press at `from`, walk the pointer to `to` over `steps` moves, release.
   * The app gets a settled frame between every step, because drag previews
   * only appear on a repaint.
   *
   * `to` is either another node, or `{dx, dy}` — a displacement in logical
   * pixels from where the drag started. Use the delta form for anything on a
   * canvas, where the destination is a position and not a control. A delta
   * that would end outside the window is an error, not a shorter drag.
   */
  drag(
    from: Node,
    to: Node | { dx: number; dy: number },
    options?: ActOptions & { steps?: number },
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
