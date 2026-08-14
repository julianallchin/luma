/** Every `skills/<name>/SKILL.md` bundled into the app at build time.
 *
 * The webview has no filesystem, so skills cannot be discovered at runtime the
 * way a CLI agent would. Vite inlines them here instead; adding a directory with
 * a `SKILL.md` is the whole registration step. Sorted by path so the derived
 * tool description is byte-stable across builds. */
const modules = import.meta.glob("./*/SKILL.md", {
	query: "?raw",
	import: "default",
	eager: true,
}) as Record<string, string>;

export const BUNDLED_SKILL_SOURCES: readonly string[] = Object.keys(modules)
	.sort()
	.map((path) => modules[path]);
