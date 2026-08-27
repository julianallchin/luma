// The whole of the model-facing API, written once in the language the model
// writes. Rust exposes exactly three bindings — `__call`, `__log`, `__help` —
// and everything below is ordinary JavaScript on top of them.
//
// Keeping the surface here rather than in Rust is what makes the API and its
// `.d.ts` cheap to keep in step: there is one list of members, and a test
// compares it against the declaration file.

(() => {
  const call = (cmd, args) => JSON.parse(__call(cmd, JSON.stringify(args ?? {})));

  const predicate = (query) => {
    if (typeof query === "function") return query;
    return (node) =>
      (query.role === undefined || node.role === query.role) &&
      (query.label === undefined || node.label === query.label);
  };

  // A `find` that matched nothing hands back `undefined`, and passing that on
  // would surface as a serde error about a missing field several layers away.
  // Say it where it happened instead.
  const node = (value, which) => {
    if (value === undefined || value === null || typeof value.id !== "number") {
      throw new Error(
        `expected a node from app.snapshot() for \`${which}\`, got ${JSON.stringify(value)}`,
      );
    }
    return value;
  };

  // A drag ends either on another node or at a displacement. Branch before
  // `node`, which would reject `{dx, dy}` — but keep falling through to it, so
  // a target with a typo'd key still throws about the node it is not.
  const target = (value) => {
    if (value !== null && typeof value === "object" &&
        typeof value.dx === "number" && typeof value.dy === "number") {
      // A displacement carries dx and dy and nothing else. Anything more is
      // almost always `app.drag(node, {dx, dy, modifiers})` — the options
      // belong in a *third* argument, and taken as part of the target they
      // would be silently dropped and the gesture quietly wrong.
      const extra = Object.keys(value).filter((key) => key !== "dx" && key !== "dy");
      if (extra.length > 0) {
        throw new Error(
          `a drag target is {dx, dy}; \`${extra.join(", ")}\` belongs in the ` +
          `options argument: app.drag(from, {dx, dy}, { ${extra[0]}: … })`,
        );
      }
      return { by: { dx: value.dx, dy: value.dy } };
    }
    return { node: node(value, "to") };
  };

  // A gesture origin is a node, or a bare {x, y} point in the window — a
  // canvas has positions where no control is (an empty spot to pan from, or
  // to sweep a marquee from), and the wire carries whichever was given.
  const pointer = (value, which) =>
    value !== null && typeof value === "object" &&
    typeof value.x === "number" && typeof value.y === "number"
      ? { at: { x: value.x, y: value.y } }
      : { node: node(value, which) };

  const snapshot = () => {
    const shot = call("snapshot");
    shot.find = (query) => shot.nodes.find(predicate(query));
    shot.findAll = (query) => shot.nodes.filter(predicate(query));
    return shot;
  };

  // Options are picked out by name rather than spread, so a misspelt one is a
  // thrown error and not a silently ignored instruction. A driver that thinks
  // it passed `restale` and did not would misread every result after it.
  const options = (given, allowed) => {
    const out = {};
    for (const [key, value] of Object.entries(given ?? {})) {
      const wire = allowed[key];
      if (wire === undefined) {
        throw new Error(
          `unknown option \`${key}\`; expected one of ${Object.keys(allowed).join(", ")}`,
        );
      }
      if (value !== undefined) out[wire] = value;
    }
    return out;
  };

  globalThis.app = {
    snapshot,
    click: (target, opts) =>
      call("click", {
        node: node(target, "node"),
        ...options(opts, {
          restale: "restale",
          modifiers: "modifiers",
          button: "button",
          count: "count",
        }),
      }),
    drag: (from, to, opts) =>
      call("drag", {
        from: pointer(from, "from"),
        to: target(to),
        ...options(opts, {
          restale: "restale",
          steps: "steps",
          modifiers: "modifiers",
          button: "button",
        }),
      }),
    type: (target, text, opts) =>
      call("type", {
        node: node(target, "node"),
        text,
        ...options(opts, { restale: "restale", modifiers: "modifiers" }),
      }),
    key: (keys) => call("key", { keys }),
    action: (name, payload) => call("action", { name, payload: payload ?? null }),
    frames: (n, opts) =>
      call("frames", { n: n ?? 1, ...options(opts, { waitMs: "wait_ms" }) }),
    scroll: (where_, opts) =>
      call("scroll", {
        at: pointer(where_, "where"),
        ...options(opts, {
          dx: "dx",
          dy: "dy",
          steps: "steps",
          modifiers: "modifiers",
          restale: "restale",
        }),
      }),
    timings: () => call("timings", {}),
    screenshot: (opts) =>
      call("screenshot", options(opts, { node: "node", restale: "restale" })),
    help: () => __help(),
  };

  const line = (args) =>
    args
      .map((arg) =>
        typeof arg === "string" ? arg : JSON.stringify(arg) ?? String(arg),
      )
      .join(" ");

  globalThis.console = {
    log: (...args) => __log(line(args)),
    info: (...args) => __log(line(args)),
    warn: (...args) => __log(line(args)),
    error: (...args) => __log(line(args)),
  };
})();
