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
        ...options(opts, { restale: "restale" }),
      }),
    drag: (from, to, opts) =>
      call("drag", {
        from: node(from, "from"),
        to: node(to, "to"),
        ...options(opts, { restale: "restale", steps: "steps" }),
      }),
    type: (target, text, opts) =>
      call("type", {
        node: node(target, "node"),
        text,
        ...options(opts, { restale: "restale" }),
      }),
    key: (keys) => call("key", { keys }),
    action: (name, payload) => call("action", { name, payload: payload ?? null }),
    frames: (n, opts) =>
      call("frames", { n: n ?? 1, ...options(opts, { waitMs: "wait_ms" }) }),
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
