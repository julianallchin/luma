// What a `.test.js` file is written against: `fixture`, `test`, `expect`,
// `assert`. `luma-test` loads this twice per file.
//
// First with no app and `__only` unset: `test(...)` only records, so the
// runner learns the file's fixture and test names — and a syntax error fails
// here, before any app opens. Then once per test, in that test's harness,
// with `__only` set to its name: `test(...)` runs that body and no other.
//
// A body runs synchronously and fails by throwing; the runner reads the
// error, its stack (for the file's line) and the last frame the test saw.

(() => {
  const only = globalThis.__only;
  const registry = { fixture: null, tests: [] };
  globalThis.__registry = registry;
  globalThis.__ran = false;

  globalThis.fixture = (spec) => {
    if (spec === null || typeof spec !== "object" || Array.isArray(spec)) {
      throw new Error("fixture(spec) takes an object, e.g. fixture({ track: false })");
    }
    if (registry.fixture !== null) throw new Error("fixture() is called once per file");
    registry.fixture = spec;
  };

  const OPTIONS = ["fixture", "timeoutMs", "pixel"];
  const define = (skip) => (name, opts, body) => {
    if (typeof opts === "function") [opts, body] = [{}, opts];
    if (typeof name !== "string" || name === "") throw new Error("test(name, fn): name must be a string");
    if (typeof body !== "function") throw new Error(`test(${JSON.stringify(name)}): no body`);
    for (const key of Object.keys(opts ?? {})) {
      if (!OPTIONS.includes(key)) {
        throw new Error(`test(${JSON.stringify(name)}): unknown option \`${key}\`; expected ${OPTIONS.join(", ")}`);
      }
    }
    if (only === undefined) {
      if (registry.tests.some((test) => test.name === name)) {
        throw new Error(`two tests named ${JSON.stringify(name)}`);
      }
      registry.tests.push({
        name,
        skip,
        fixture: opts?.fixture ?? null,
        timeoutMs: opts?.timeoutMs ?? null,
        pixel: opts?.pixel === true,
      });
    } else if (name === only && !skip) {
      globalThis.__ran = true;
      body();
    }
  };
  globalThis.test = define(false);
  globalThis.test.skip = define(true);

  // -- assertions -------------------------------------------------------------

  const show = (value) => {
    if (value instanceof RegExp) return String(value);
    let text;
    try {
      text = JSON.stringify(value);
    } catch {
      text = undefined;
    }
    text = text === undefined ? String(value) : text;
    return text.length > 400 ? `${text.slice(0, 400)}…` : text;
  };

  const equal = (a, b) => {
    if (Object.is(a, b)) return true;
    if (typeof a !== "object" || typeof b !== "object" || a === null || b === null) return false;
    if (Array.isArray(a) !== Array.isArray(b)) return false;
    const keys = Object.keys(a);
    if (keys.length !== Object.keys(b).length) return false;
    return keys.every((key) => Object.prototype.hasOwnProperty.call(b, key) && equal(a[key], b[key]));
  };

  const fail = (message) => {
    throw new Error(message);
  };

  globalThis.expect = (actual) => ({
    toBe: (expected) =>
      Object.is(actual, expected) || fail(`expected ${show(actual)} to be ${show(expected)}`),
    toEqual: (expected) =>
      equal(actual, expected) || fail(`expected ${show(actual)}\n    to equal ${show(expected)}`),
    toBeTruthy: () => actual || fail(`expected ${show(actual)} to be truthy`),
    toBeGreaterThan: (bound) =>
      actual > bound || fail(`expected ${show(actual)} to be greater than ${show(bound)}`),
    toBeLessThan: (bound) =>
      actual < bound || fail(`expected ${show(actual)} to be less than ${show(bound)}`),
    toContain: (item) =>
      (typeof actual === "string"
        ? actual.includes(item)
        : Array.isArray(actual) && actual.some((entry) => equal(entry, item))) ||
      fail(`expected ${show(actual)} to contain ${show(item)}`),
    toMatch: (pattern) =>
      (typeof actual === "string" && new RegExp(pattern).test(actual)) ||
      fail(`expected ${show(actual)} to match ${show(pattern)}`),
  });

  globalThis.assert = (condition, message) => {
    if (!condition) fail(message ?? "assertion failed");
  };

  // -- what a failure report reads back --------------------------------------

  // Only in a harness: registration has no app to wrap.
  if (typeof app !== "undefined") {
    const snapshot = app.snapshot;
    app.snapshot = (options) => (globalThis.__lastFrame = snapshot(options));
    const screenshot = app.screenshot;
    app.screenshot = (options) => {
      const shot = screenshot(options);
      globalThis.__lastShot = shot.path;
      return shot;
    };
  }
})();
