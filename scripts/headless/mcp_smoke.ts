/**
 * End-to-end check of the MCP server: `bun run scripts/headless/mcp_smoke.ts`.
 *
 * Speaks the wire an MCP client speaks — newline-delimited JSON-RPC on the
 * binary's stdio — rather than going through `shim.ts`, because the point is to
 * prove the *protocol* works, not the services underneath it (`smoke.ts` and
 * `e2e.ts` already cover those).
 *
 * Isolation matches `e2e.ts`: a scratch config dir seeded from a copy of the
 * real `luma.db`, `tracks/` symlinked (tens of GB, read-only on every path
 * here), authored rows re-homed to a synthetic fixture owner so the write gate
 * is exercised without copying an auth secret. The cache dir points at the real
 * one so the managed venv is reused rather than rebuilt.
 */

import { copyFileSync, existsSync, mkdtempSync, rmSync, symlinkSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Database } from "bun:sqlite";
import { REAL_CONFIG_DIR as REAL_CONFIG, startMcpServer, textOf } from "./mcp-client";
import { normalizeScratchLibraryToPrincipal } from "./scratch-library";

const FIXTURE_PRINCIPAL = "headless-mcp-owner";
/** What this fake client tells `open` is driving it, so the revisions it writes
 * name a model as well as a client. */
const SESSION_MODEL = "claude-opus-5";

// -----------------------------------------------------------------------------
// Assertions
// -----------------------------------------------------------------------------

type Outcome = "pass" | "fail" | "skip";
const results: { name: string; outcome: Outcome; detail?: string }[] = [];

function record(name: string, outcome: Outcome, detail?: string) {
	results.push({ name, outcome, detail });
	const mark = outcome === "pass" ? "PASS" : outcome === "fail" ? "FAIL" : "SKIP";
	console.log(`  ${mark}  ${name}${detail ? ` — ${detail}` : ""}`);
}

function check(name: string, cond: boolean, detail?: string) {
	record(name, cond ? "pass" : "fail", cond ? undefined : (detail ?? "assertion failed"));
}

// -----------------------------------------------------------------------------
// Scratch library
// -----------------------------------------------------------------------------

const scratch = mkdtempSync(join(tmpdir(), "luma-mcp-"));
let hasRealDb = false;
for (const suffix of ["", "-wal", "-shm"]) {
	const src = join(REAL_CONFIG, `luma.db${suffix}`);
	if (existsSync(src)) {
		copyFileSync(src, join(scratch, `luma.db${suffix}`));
		if (suffix === "") hasRealDb = true;
	}
}
if (hasRealDb) normalizeScratchLibraryToPrincipal(join(scratch, "luma.db"), FIXTURE_PRINCIPAL);
if (existsSync(join(REAL_CONFIG, "tracks"))) {
	symlinkSync(join(REAL_CONFIG, "tracks"), join(scratch, "tracks"));
}
console.log(`scratch config dir: ${scratch}`);
if (!hasRealDb) console.log("no real luma.db — data-dependent checks will skip");

// -----------------------------------------------------------------------------
// The run
// -----------------------------------------------------------------------------

const server = startMcpServer({
	configDir: scratch,
	fixturePrincipal: FIXTURE_PRINCIPAL,
	clientInfo: { name: "mcp_smoke", version: "0" },
});
try {
	console.log("\n[handshake]");
	const initialized = await server.handshake();
	check("initialize names the server", initialized?.serverInfo?.name === "luma");
	check("initialize declares tools", initialized?.capabilities?.tools !== undefined);
	check("ping answers", JSON.stringify(await server.request("ping")) === "{}");

	const listed = await server.request<{ tools: { name: string }[] }>("tools/list");
	const names = listed.tools.map((t) => t.name).sort();
	check(
		"tools/list is the contract",
		JSON.stringify(names) === '["cancel","find","open","python","reset","skill"]',
		names.join(","),
	);

	// The playbooks are the other half of what makes the in-app copilot good at
	// this, and they reach an external agent three ways: the `skill` tool's
	// description (the model's menu), `prompts/*` (the human's slash menu), and
	// `open`'s reply. All three read one registry, so all three must agree.
	console.log("\n[skills]");
	check("initialize declares prompts", initialized?.capabilities?.prompts !== undefined);
	const skillTool = listed.tools.find((t) => t.name === "skill") as
		| { description?: string }
		| undefined;
	check(
		"the skill tool carries the listing",
		(skillTool?.description ?? "").includes("<available_skills>"),
		(skillTool?.description ?? "").slice(0, 120),
	);
	// The listing is name + description only. A `<location>` would make the
	// text machine-specific, and nothing fetches by path — the `skill` tool and
	// `prompts/get` both take a name.
	check(
		"the listing carries no filesystem path",
		!(skillTool?.description ?? "").includes("SKILL.md"),
		(skillTool?.description ?? "").slice(0, 200),
	);

	const prompts = await server.request<{ prompts: { name: string; description: string }[] }>(
		"prompts/list",
	);
	check("prompts/list is every skill", prompts.prompts.length === 10, `${prompts.prompts.length}`);
	check(
		"a prompt carries its description, not its body",
		Boolean(prompts.prompts[0]?.description) && !("messages" in (prompts.prompts[0] ?? {})),
		JSON.stringify(prompts.prompts[0] ?? {}).slice(0, 160),
	);

	const fetched = await server.request<{ messages: { content: { text: string } }[] }>(
		"prompts/get",
		{ name: "heavy-bass" },
	);
	const promptBody = fetched.messages[0]?.content?.text ?? "";
	check(
		"prompts/get returns the skill envelope",
		promptBody.startsWith('<skill name="heavy-bass"'),
		promptBody.slice(0, 120),
	);

	const loaded = await server.callTool("skill", { name: "heavy-bass" });
	check(
		"the skill tool returns the same envelope",
		!loaded.isError && textOf(loaded) === promptBody,
		textOf(loaded).slice(0, 160),
	);
	const nonsense = await server.callTool("skill", { name: "polka" });
	check(
		"an unknown skill lists the real ones",
		nonsense.isError === true && textOf(nonsense).includes("heavy-bass"),
		textOf(nonsense).slice(0, 160),
	);

	// A lookup must never author. `find` is the whole reason `open` no longer
	// has a listing mode: `open` pins a thread and mints a score for a track new
	// to the room, so resolving an id through it wrote a revision per lookup.
	console.log("\n[find]");
	const revisions = () => {
		if (!hasRealDb) return 0;
		const library = new Database(join(scratch, "luma.db"), { readonly: true });
		try {
			return (
				library.query<{ n: number }, []>("SELECT count(*) AS n FROM authored_revisions").get()
					?.n ?? 0
			);
		} finally {
			library.close();
		}
	};
	const before = revisions();
	const listing = await server.callTool("find");
	check("find with no args lists the library", textOf(listing).includes("venues:"));
	const trackId = textOf(listing).match(/^ {2}(\S+) {2}.+$/m)?.[1];
	const venueIds = (textOf(listing).split(/\n\d+ venues:\n/)[1] ?? "")
		.split("\n")
		.map((line) => line.match(/^ {2}(\S+) {2}/)?.[1])
		.filter((id): id is string => Boolean(id));

	if (!hasRealDb || !trackId) {
		record("find narrows to one track", "skip", "no track in the scratch library");
	} else {
		const byId = await server.callTool("find", { track: trackId });
		check(
			"find matches a track by id",
			textOf(byId).startsWith("1 tracks:") && textOf(byId).includes(trackId),
			textOf(byId).slice(0, 200),
		);
		const nothing = await server.callTool("find", { track: "\u0000no such track" });
		check("find that matches nothing says so", textOf(nothing).startsWith("0 tracks:"), textOf(nothing).slice(0, 120));
	}
	check("find writes nothing", revisions() === before, `${before} -> ${revisions()} revisions`);

	console.log("\n[open]");

	if (!hasRealDb || !trackId) {
		record("open a track", "skip", "no track in the scratch library");
	} else {
		const opened = await server.callTool("open", {
			track_id: trackId,
			model: SESSION_MODEL,
		});
		const openedText = textOf(opened);
		check("open binds a track", !opened.isError && openedText.startsWith("opened "), openedText.slice(0, 200));
		check("open returns the binding catalog", openedText.includes("luma."), openedText.slice(0, 400));
		check(
			"open ends with the skills listing",
			openedText.trimEnd().endsWith("</available_skills>"),
			openedText.slice(-200),
		);
		console.log(`  ${openedText.split("\n").slice(0, 3).join(" | ")}`);

		console.log("\n[python]");
		const shape = await server.callTool("python", { code: "luma.venue.positions.shape" });
		check("a cell returns its last expression", !shape.isError, textOf(shape).slice(0, 400));
		console.log(`  luma.venue.positions.shape -> ${textOf(shape).trim()}`);

		const pieces = await server.callTool("python", {
			code: "sorted({p['kind'] for p in luma.venue.pieces})",
		});
		check("the set design is bound", !pieces.isError, textOf(pieces).slice(0, 400));
		console.log(`  luma.venue.pieces kinds -> ${textOf(pieces).trim()}`);

		const figure = await server.callTool("python", {
			code: [
				"import numpy as np, matplotlib.pyplot as plt",
				"fig, ax = plt.subplots(figsize=(4, 2))",
				"ax.plot(np.linspace(0, 1, 64), np.sin(np.linspace(0, 6.28, 64)))",
				"fig",
			].join("\n"),
		});
		const images = figure.content.filter((block) => block.type === "image");
		check("a figure comes back as an image block", images.length === 1, `${images.length} images`);
		check(
			"the image is base64 png",
			images[0]?.mimeType === "image/png" && (images[0]?.data?.length ?? 0) > 1000,
			`${images[0]?.mimeType} ${images[0]?.data?.length ?? 0}b`,
		);

		const persisted = await server.callTool("python", { code: "'kernel' + ' persists'" });
		check("the kernel is persistent", textOf(persisted).includes("kernel persists"), textOf(persisted).slice(0, 200));

		// Authorship. An external client is not the operator: everything this
		// session writes must be labelled as the client (and the model it says
		// is driving), not as `user`.
		console.log("\n[authorship]");
		const edit = await server.callTool("python", {
			code: [
				"draft = luma.track.edit()",
				"if luma.track.clips:",
				"    seed = luma.track.clips[0]",
				"    z = max(c.z for c in luma.track.clips) + 1",
				"    draft.add_clip(seed.pattern_id, seconds=(seed.start_s, seed.end_s), z=z, blend=seed.blend, args=dict(seed.args))",
				"else:",
				"    pattern = luma.patterns.summaries[0]",
				"    draft.add_clip(pattern.id, seconds=(0.0, min(4.0, luma.track.duration_s)), z=0)",
				"applied = draft.apply()",
				"applied.applied",
			].join("\n"),
		});
		if (edit.isError) {
			record("an MCP edit is attributed to the client", "skip", textOf(edit).slice(0, 300));
		} else {
			const library = new Database(join(scratch, "luma.db"), { readonly: true });
			try {
				const revision = library
					.query<{ actor: string; operation_kind: string }, []>(
						`SELECT actor, operation_kind FROM authored_revisions
						 WHERE operation_kind = 'score_edit'
						 ORDER BY created_at DESC, rowid DESC LIMIT 1`,
					)
					.get();
				check(
					"an MCP edit is attributed to the client and its model",
					revision?.actor === `client:mcp_smoke/0:${SESSION_MODEL}`,
					`actor=${revision?.actor ?? "no score_edit revision"}`,
				);
				const thread = library
					.query<{ actor: string | null }, []>(
						"SELECT actor FROM agent_threads ORDER BY updated_at DESC LIMIT 1",
					)
					.get();
				check(
					"open stamps the session's writer on the thread",
					thread?.actor === `client:mcp_smoke/0:${SESSION_MODEL}`,
					`actor=${thread?.actor ?? "null"}`,
				);
			} finally {
				library.close();
			}
		}

		console.log("\n[cancel + reset]");
		const idle = await server.callTool("cancel");
		check("cancel with nothing running says so", textOf(idle).includes("no cell running"), textOf(idle));

		const reopened = await server.callTool("reset");
		check("reset rebinds the track", !reopened.isError && textOf(reopened).startsWith("opened "), textOf(reopened).slice(0, 200));
		// A track with several scores in one venue must not pick a different
		// one each time: the binding is the app's own "most recently updated".
		const scoreOf = (text: string) => text.match(/^venue \S+, score (\S+)$/m)?.[1];
		check(
			"reopening binds the same score",
			Boolean(scoreOf(openedText)) && scoreOf(openedText) === scoreOf(textOf(reopened)),
			`${scoreOf(openedText)} vs ${scoreOf(textOf(reopened))}`,
		);
		const gone = await server.callTool("python", { code: "'kernel' + ' persists'" });
		check("reset threw the namespace away", !gone.isError, textOf(gone).slice(0, 200));

		// A venue the track has never been scored in is still a room to look at:
		// `open` makes the membership instead of refusing.
		console.log("\n[open a venue the track is not in]");
		const bound = textOf(reopened).match(/^venue (\S+), score /m)?.[1];
		const other = venueIds.find((id) => id !== bound);
		if (!other) {
			record("open a venue without a score", "skip", "the library has only one venue");
		} else {
			const elsewhere = await server.callTool("open", { track_id: trackId, venue_id: other });
			const elsewhereText = textOf(elsewhere);
			check(
				"open binds a venue the track has no score in",
				!elsewhere.isError && elsewhereText.includes(`venue ${other}, score `),
				elsewhereText.slice(0, 300),
			);
			const room = await server.callTool("python", { code: "luma.venue.positions.shape" });
			check("the venue is describable there", !room.isError, textOf(room).slice(0, 300));
			const clips = await server.callTool("python", { code: "len(luma.track.clips)" });
			check("its timeline starts empty", textOf(clips).trim() === "0", textOf(clips).slice(0, 300));
		}
	}

	console.log("\n[errors are results, not transport failures]");
	const raised = await server.callTool("python", { code: "1 / 0" });
	check("a traceback is an isError result", raised.isError === true && textOf(raised).includes("ZeroDivisionError"), textOf(raised).slice(0, 200));
	const nameless = await server.callTool("open");
	check(
		"open with no track is an error, not a listing",
		nameless.isError === true && textOf(nameless).includes("find"),
		textOf(nameless).slice(0, 200),
	);
	const unknown = await server.callTool("nope");
	check("an unknown tool is an isError result", unknown.isError === true, JSON.stringify(unknown).slice(0, 200));
} finally {
	await server.close();
	rmSync(scratch, { recursive: true, force: true });
}

const failed = results.filter((r) => r.outcome === "fail").length;
const passed = results.filter((r) => r.outcome === "pass").length;
const skipped = results.filter((r) => r.outcome === "skip").length;
console.log(`\n${passed} passed, ${failed} failed, ${skipped} skipped`);
process.exit(failed > 0 ? 1 : 0);
