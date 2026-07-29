/**
 * End-to-end smoke test for the headless harness: `bun run scripts/headless/smoke.ts`.
 *
 * Runs against a *copy* of the real `luma.db` in a scratch config dir — the
 * source is only ever read, never opened for writing (migrations, WAL, and
 * every mutation below land on the copy). With no real library present the
 * data-dependent checks are skipped rather than failed.
 */

import { copyFileSync, existsSync, mkdtempSync, rmSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";
import { type Harness, startHarness } from "./shim";

// -----------------------------------------------------------------------------
// Tiny assertion harness
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

async function section(name: string, fn: () => Promise<void>) {
	console.log(`\n[${name}]`);
	try {
		await fn();
	} catch (e) {
		record(name, "fail", e instanceof Error ? e.message : String(e));
	}
}

// -----------------------------------------------------------------------------
// Scratch config dir
// -----------------------------------------------------------------------------

const REAL_CONFIG = join(homedir(), "Library/Application Support/com.luma.luma");
const scratch = mkdtempSync(join(tmpdir(), "luma-headless-"));
let hasRealDb = false;

for (const suffix of ["", "-wal", "-shm"]) {
	const src = join(REAL_CONFIG, `luma.db${suffix}`);
	if (existsSync(src)) {
		copyFileSync(src, join(scratch, `luma.db${suffix}`));
		if (suffix === "") hasRealDb = true;
	}
}

console.log(`scratch config dir: ${scratch}`);
console.log(hasRealDb ? "seeded from the real luma.db (read-only copy)" : "no real luma.db — data-dependent checks will skip");

let harness: Harness | undefined;
try {
	harness = await startHarness({ configDir: scratch });
	const { invoke } = harness;

	// -------------------------------------------------------------------------
	await section("agent threads", async () => {
		const thread = await invoke<{ id: string; agentKind: string; title: string | null }>(
			"agent_thread_create",
			{ input: { agentKind: "track_copilot", subjectKind: "track", subjectId: "smoke-track", title: "smoke" } },
		);
		check("create returns an id", typeof thread.id === "string" && thread.id.length > 0);
		check("create echoes agentKind", thread.agentKind === "track_copilot");

		const parts = (n: number) => [
			{ type: "text", text: `message ${n}` },
			{
				type: "tool-get_track_beats",
				toolCallId: `call-${n}`,
				state: "output-available",
				input: { trackId: "smoke-track" },
				output: { beats: [0, 0.5, 1] },
			},
		];
		const appended = await invoke<{ seq: number }[]>("agent_thread_append_messages", {
			threadId: thread.id,
			messages: [
				{ role: "user", parts: parts(1) },
				{ role: "assistant", parts: parts(2) },
				{ role: "user", parts: parts(3) },
			],
		});
		check("append returns 3 rows", appended.length === 3, `got ${appended.length}`);
		check(
			"seq is dense and ascending",
			appended.every((m, i) => m.seq === appended[0].seq + i),
			JSON.stringify(appended.map((m) => m.seq)),
		);

		const detail = await invoke<{ thread: { id: string }; messages: { role: string; parts: unknown[] }[] }>(
			"agent_thread_get",
			{ threadId: thread.id },
		);
		check("get returns the 3 messages", detail.messages.length === 3, `got ${detail.messages.length}`);
		check("roles round-trip", detail.messages.map((m) => m.role).join(",") === "user,assistant,user");
		const toolPart = detail.messages[0].parts[1] as { type: string; output: { beats: number[] } };
		check("tool part round-trips verbatim", toolPart.type === "tool-get_track_beats" && toolPart.output.beats.length === 3);

		const renamed = await invoke<{ title: string | null }>("agent_thread_rename", {
			threadId: thread.id,
			title: "renamed",
		});
		check("rename applies", renamed.title === "renamed");

		const listed = await invoke<{ id: string }[]>("agent_thread_list", { agentKind: "track_copilot" });
		check("list includes the thread", listed.some((t) => t.id === thread.id));

		const truncated = await invoke<number>("agent_thread_truncate_from", {
			threadId: thread.id,
			seq: appended[1].seq,
		});
		check("truncate deletes the tail", truncated === 2, `deleted ${truncated}`);
		const afterTruncate = await invoke<{ messages: unknown[] }>("agent_thread_get", { threadId: thread.id });
		check("one message remains", afterTruncate.messages.length === 1, `got ${afterTruncate.messages.length}`);

		const reset = await invoke<number>("agent_thread_reset", { threadId: thread.id });
		check("reset deletes the rest", reset === 1, `deleted ${reset}`);
		const afterReset = await invoke<{ messages: unknown[] }>("agent_thread_get", { threadId: thread.id });
		check("thread is empty after reset", afterReset.messages.length === 0);

		await invoke("agent_thread_delete", { threadId: thread.id });
		let gone = false;
		try {
			await invoke("agent_thread_get", { threadId: thread.id });
		} catch {
			gone = true;
		}
		check("get after delete errors", gone);
	});

	// -------------------------------------------------------------------------
	await section("error handling", async () => {
		let errored = false;
		try {
			await invoke("definitely_not_a_command", {});
		} catch (e) {
			errored = /unknown command/.test(String(e));
		}
		check("unknown command rejects", errored);

		let missingArg = false;
		try {
			await invoke("get_pattern_graph", {});
		} catch (e) {
			missingArg = /missing required argument/.test(String(e));
		}
		check("missing argument rejects", missingArg);

		// The harness must still be alive after two errors.
		await invoke("get_node_types", {});
		check("harness survives errors", true);
	});

	// -------------------------------------------------------------------------
	await section("static command surface", async () => {
		const nodeTypes = await invoke<{ type: string }[]>("get_node_types", {});
		check("get_node_types returns definitions", nodeTypes.length > 0, `${nodeTypes.length} types`);
		const thresholds = await invoke<Record<string, number>>("get_classifier_thresholds", {});
		check("get_classifier_thresholds returns a map", Object.keys(thresholds).length > 0);
	});

	// -------------------------------------------------------------------------
	await section("library reads", async () => {
		if (!hasRealDb) {
			record("library reads", "skip", "no real luma.db");
			return;
		}
		const patterns = await invoke<{ id: string; name: string }[]>("list_patterns", {});
		check("list_patterns non-empty", patterns.length > 0, `${patterns.length} patterns`);
		const venues = await invoke<{ id: string; name: string }[]>("list_venues", {});
		check("list_venues non-empty", venues.length > 0, `${venues.length} venues`);
		const tracks = await invoke<{ id: string }[]>("list_tracks_enriched", {});
		check("list_tracks_enriched non-empty", tracks.length > 0, `${tracks.length} tracks`);
	});

	// -------------------------------------------------------------------------
	await section("run_graph over a real track + venue", async () => {
		if (!hasRealDb) {
			record("run_graph", "skip", "no real luma.db");
			return;
		}

		// Pick a track that has a beat grid and a score (the score gives us the
		// venue the annotation was authored against).
		const tracks = await invoke<{ id: string; title?: string }[]>("list_tracks_enriched", {});
		const venues = await invoke<{ id: string; name: string }[]>("list_venues", {});
		let picked: { trackId: string; venueId: string; beats: number[] } | undefined;
		for (const track of tracks.slice(0, 40)) {
			const beats = await invoke<{ beats: number[] } | null>("get_track_beats", { trackId: track.id });
			if (!beats || beats.beats.length < 8) continue;
			for (const venue of venues) {
				const scores = await invoke<{ id: string }[]>("list_scores_for_track", {
					trackId: track.id,
					venueId: venue.id,
				});
				if (scores.length > 0) {
					picked = { trackId: track.id, venueId: venue.id, beats: beats.beats };
					break;
				}
			}
			if (picked) break;
		}
		if (!picked) {
			record("run_graph", "skip", "no track with both a beat grid and a score");
			return;
		}
		console.log(`  using track=${picked.trackId} venue=${picked.venueId}`);

		// A pattern whose graph actually has nodes.
		const patterns = await invoke<{ id: string; name: string }[]>("list_patterns", {});
		let graph: { nodes: unknown[]; edges: unknown[] } | undefined;
		let patternName = "";
		let patternId = "";
		for (const p of patterns) {
			const json = await invoke<string>("get_pattern_graph", { id: p.id });
			const parsed = JSON.parse(json) as { nodes?: unknown[]; edges?: unknown[] };
			if (parsed.nodes && parsed.nodes.length > 0) {
				graph = { nodes: parsed.nodes, edges: parsed.edges ?? [] };
				patternName = p.name;
				patternId = p.id;
				break;
			}
		}
		if (!graph) {
			record("run_graph", "skip", "no pattern with a non-empty graph");
			return;
		}
		console.log(`  using pattern=${patternName} (${graph.nodes.length} nodes)`);

		const startTime = picked.beats[0];
		const endTime = picked.beats[Math.min(16, picked.beats.length - 1)];
		const run = await invoke<{
			views: Record<string, unknown>;
			melSpecs: Record<string, unknown>;
			colorViews: Record<string, unknown>;
			universeState: { fixtures?: unknown[] } | null;
		}>("run_graph", {
			graph,
			context: { trackId: picked.trackId, venueId: picked.venueId, startTime, endTime },
			includeMelSpecs: false,
		});

		check("run_graph returns a views map", run.views !== undefined && typeof run.views === "object");
		check("run_graph returns a universeState", run.universeState !== null && run.universeState !== undefined);
		check("mel specs are skipped when not requested", Object.keys(run.melSpecs ?? {}).length === 0);

		const withMel = await invoke<{ melSpecs: Record<string, unknown> }>("run_graph", {
			graph,
			context: { trackId: picked.trackId, venueId: picked.venueId, startTime, endTime },
			includeMelSpecs: true,
		});
		check("includeMelSpecs is honored", withMel.melSpecs !== undefined && typeof withMel.melSpecs === "object");

		// Venue reads the agents' ask-venue tool depends on.
		const fixtures = await invoke<unknown[]>("get_patched_fixtures", { venueId: picked.venueId });
		const hierarchy = await invoke<unknown[]>("get_grouped_hierarchy", { venueId: picked.venueId });
		check("get_patched_fixtures returns an array", Array.isArray(fixtures), `${fixtures.length} fixtures`);
		check("get_grouped_hierarchy returns an array", Array.isArray(hierarchy), `${hierarchy.length} groups`);

		// Heatmap previews — the agents' "look at the output" tools.
		const preview = await invoke<{ width: number; height: number; pixels: number[] | string }>(
			"preview_pattern_image",
			{ patternId, trackId: picked.trackId, venueId: picked.venueId, startTime, endTime },
		);
		check(
			"preview_pattern_image returns a sized image",
			preview.width > 0 && preview.height > 0,
			`${preview.width}x${preview.height}`,
		);

		const graphPreview = await invoke<{ width: number; height: number }>("preview_graph_image", {
			graph,
			trackId: picked.trackId,
			venueId: picked.venueId,
			startTime,
			endTime,
		});
		check("preview_graph_image returns a sized image", graphPreview.width > 0 && graphPreview.height > 0);

		try {
			const composite = await invoke<{ width: number; height: number }>("view_composite_image", {
				trackId: picked.trackId,
				startTime,
				endTime,
			});
			check("view_composite_image returns a sized image", composite.width > 0 && composite.height > 0);
		} catch (e) {
			// Legitimately unavailable when the track has no annotations.
			record("view_composite_image", /No annotations|No score/.test(String(e)) ? "skip" : "fail", String(e));
		}
	});
} finally {
	await harness?.close();
	rmSync(scratch, { recursive: true, force: true });
}

// -----------------------------------------------------------------------------

const failed = results.filter((r) => r.outcome === "fail");
const skipped = results.filter((r) => r.outcome === "skip");
console.log(
	`\n${results.length - failed.length - skipped.length} passed, ${failed.length} failed, ${skipped.length} skipped`,
);
for (const f of failed) console.log(`  FAILED: ${f.name} — ${f.detail}`);
console.log(failed.length === 0 ? "\nSMOKE: PASS" : "\nSMOKE: FAIL");
process.exit(failed.length === 0 ? 0 : 1);
