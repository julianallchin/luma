/**
 * End-to-end smoke test for the headless harness: `bun run scripts/headless/smoke.ts`.
 *
 * Runs against a *copy* of the real `luma.db` in a scratch config dir — the
 * source is only ever read, never opened for writing (migrations, WAL, and
 * every mutation below land on the copy). With no real library present the
 * data-dependent checks are skipped rather than failed.
 */

import { copyFileSync, existsSync, mkdtempSync, realpathSync, rmSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { isAbsolute, join, sep } from "node:path";
import type { TrackEditResult } from "../../src/bindings/schema";
import { normalizeScratchLibraryToGuest } from "./scratch-library";
import { type Harness, startHarness } from "./shim";

// -----------------------------------------------------------------------------
// Tiny assertion harness
// -----------------------------------------------------------------------------

type Outcome = "pass" | "fail" | "skip";
type AuthoredHistoryPage = {
	entries: { revisionId: string }[];
	nextCursor: string | null;
};
type TranscriptMessage = { id: string; seq: number };
type TranscriptAppendOutcome =
	| {
			status: "appended";
			headMessageId: string;
			messages: TranscriptMessage[];
	  }
	| {
			status: "head_moved";
			currentHeadMessageId: string | null;
	  };

function appended(outcome: TranscriptAppendOutcome): Extract<TranscriptAppendOutcome, { status: "appended" }> {
	if (outcome.status !== "appended") {
		throw new Error(`transcript head moved to ${outcome.currentHeadMessageId ?? "empty"}`);
	}
	return outcome;
}
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
const scratchReal = realpathSync(scratch);
let hasRealDb = false;

for (const suffix of ["", "-wal", "-shm"]) {
	const src = join(REAL_CONFIG, `luma.db${suffix}`);
	if (existsSync(src)) {
		copyFileSync(src, join(scratch, `luma.db${suffix}`));
		if (suffix === "") hasRealDb = true;
	}
}

// The harness intentionally starts signed out. Normalize ownership only in
// the disposable copy so authored-scope checks remain real without copying a
// live Supabase session into the test directory.
if (hasRealDb) {
	normalizeScratchLibraryToGuest(join(scratch, "luma.db"));
}

console.log(`scratch config dir: ${scratch}`);
console.log(hasRealDb ? "seeded from the real luma.db (read-only copy)" : "no real luma.db — data-dependent checks will skip");

let harness: Harness | undefined;
try {
	harness = await startHarness({ configDir: scratch });
	const { invoke } = harness;

	// -------------------------------------------------------------------------
	await section("agent threads", async () => {
		const patterns = await invoke<{ id: string; uid: string | null }[]>("list_patterns", {});
		const pattern = patterns.find((candidate) => candidate.uid === null);
		if (!pattern) {
			record("agent threads", "skip", "no signed-out pattern available");
			return;
		}
		// The graph projection and canonical authored head advance atomically. A
		// graph error here is therefore a real regression, not stale materialization
		// for this test to silently skip.
		const graphDocument = await invoke<{
			implementationId: string;
			revision: string;
			graph: { nodes: unknown[]; edges: unknown[]; args: unknown[] };
		}>("get_pattern_graph_document", {
			id: pattern.id,
			implementationId: null,
		});
		const thread = await invoke<{ id: string; agentKind: string; title: string | null }>(
			"agent_thread_create",
			{
				input: {
					requestId: crypto.randomUUID(),
					agentKind: "pattern_graph",
					subjectKind: "pattern",
					subjectId: pattern.id,
					implementationId: graphDocument.implementationId,
					title: "smoke",
				},
			},
		);
		check("create returns an id", typeof thread.id === "string" && thread.id.length > 0);
		check("create echoes agentKind", thread.agentKind === "pattern_graph");

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
		const firstAppend = appended(await invoke<TranscriptAppendOutcome>("agent_thread_append_messages", {
			threadId: thread.id,
			input: {
				operationId: "smoke-initial-transcript",
				expectedHeadMessageId: null,
				messages: [
					{ role: "user", parts: parts(1) },
					{ role: "assistant", parts: parts(2) },
					{ role: "user", parts: parts(3) },
				],
			},
		}));
		const firstMessages = firstAppend.messages;
		check("append returns 3 rows", firstMessages.length === 3, `got ${firstMessages.length}`);
		check(
			"seq is dense and ascending",
			firstMessages.every((m, i) => m.seq === firstMessages[0].seq + i),
			JSON.stringify(firstMessages.map((m) => m.seq)),
		);
		const replayed = appended(await invoke<TranscriptAppendOutcome>("agent_thread_append_messages", {
			threadId: thread.id,
			input: {
				operationId: "smoke-initial-transcript",
				expectedHeadMessageId: null,
				messages: [
					{ role: "user", parts: parts(1) },
					{ role: "assistant", parts: parts(2) },
					{ role: "user", parts: parts(3) },
				],
			},
		}));
		check("append replay returns the exact rows", JSON.stringify(replayed.messages) === JSON.stringify(firstMessages));

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

		const listed = await invoke<{ id: string }[]>("agent_thread_list", { agentKind: "pattern_graph" });
		check("list includes the thread", listed.some((t) => t.id === thread.id));

		const next = appended(await invoke<TranscriptAppendOutcome>("agent_thread_append_messages", {
			threadId: thread.id,
			input: {
				operationId: "smoke-next-message",
				expectedHeadMessageId: firstAppend.headMessageId,
				messages: [{ role: "user", parts: parts(4) }],
			},
		}));
		check("next append is dense", next.messages[0]?.seq === firstMessages[2].seq + 1);
		const afterAppend = await invoke<{ messages: unknown[] }>("agent_thread_get", { threadId: thread.id });
		check("four messages remain", afterAppend.messages.length === 4, `got ${afterAppend.messages.length}`);

		const assistantMessageId = crypto.randomUUID();
		const prepared = await invoke<{
			preparedRevisionId: string;
			document: { kind: string; revision: string };
		}>("authored_state_prepare_turn", {
			input: {
				threadId: thread.id,
				assistantMessageId,
				graph: graphDocument.graph,
			},
		});
		check("turn prepare captures a detached revision", prepared.preparedRevisionId.length > 0);
		check("turn prepare returns a graph projection", prepared.document.kind === "pattern_graph");

		await invoke("agent_thread_append_messages", {
			threadId: thread.id,
			input: {
				operationId: "smoke-final-assistant",
				expectedHeadMessageId: next.headMessageId,
				messages: [
					{
						id: assistantMessageId,
						role: "assistant",
						parts: [{ type: "text", text: "done" }],
					},
				],
			},
		});
		const finalized = await invoke<
			| {
					status: "committed";
					revisionId: string;
					appliedToCurrentProjection: boolean;
					document: { kind: string; revision: string };
			  }
			| { status: "conflicted"; conflicts: unknown[] }
		>("authored_state_finalize_turn", {
			input: {
				threadId: thread.id,
				assistantMessageId,
				preparedRevisionId: prepared.preparedRevisionId,
			},
		});
		if (finalized.status !== "committed") {
			throw new Error(`unchanged graph turn conflicted: ${JSON.stringify(finalized.conflicts)}`);
		}
		check("turn finalize advances the document head", finalized.revisionId.length > 0);
		check("turn finalize is the current projection", finalized.appliedToCurrentProjection);
		check("turn finalize preserves the graph", finalized.document.revision === graphDocument.revision);

		const recovered = await invoke<unknown[]>("authored_state_recover_turns", { threadId: thread.id });
		check("recovery is idempotent after finalize", recovered.length === 0, `got ${recovered.length}`);
		const historyPage = await invoke<AuthoredHistoryPage>("authored_state_list_history", {
			threadId: thread.id,
			cursor: null,
			limit: 20,
		});
		const history = historyPage.entries;
		check("history contains the finalized turn", history.some((entry) => entry.revisionId === finalized.revisionId));
		const initialRevision = history.at(-1)?.revisionId;
		if (!initialRevision) throw new Error("authored history did not contain an initial revision");
		const restoreOperationId = crypto.randomUUID();
		const restored = await invoke<{ revisionId: string; appliedToCurrentProjection: boolean }>(
			"authored_state_restore",
			{
				input: {
					threadId: thread.id,
					targetRevisionId: initialRevision,
					operationId: restoreOperationId,
					mode: "state_only",
				},
			},
		);
		const restoredAgain = await invoke<{ revisionId: string; appliedToCurrentProjection: boolean }>(
			"authored_state_restore",
			{
				input: {
					threadId: thread.id,
					targetRevisionId: initialRevision,
					operationId: restoreOperationId,
					mode: "state_only",
				},
			},
		);
		check("restore retry returns the same revision", restoredAgain.revisionId === restored.revisionId);
		check("restore retry remains the current projection", restoredAgain.appliedToCurrentProjection);

		const workspace = await invoke<{
			id: string;
			path: string;
			baseRevisionId: string;
			headRevisionId: string;
		}>(
			"authored_state_create_workspace",
			{
				input: {
					threadId: thread.id,
					requestId: crypto.randomUUID(),
					expectedBaseRevisionId: finalized.revisionId,
				},
			},
		);
		check(
			"workspace starts at the orchestrator-selected historical base",
			workspace.baseRevisionId === finalized.revisionId &&
				workspace.headRevisionId === finalized.revisionId,
		);
		check(
			"workspace exposes its trusted bounded snapshot",
			isAbsolute(workspace.path) &&
				workspace.path.startsWith(`${join(scratchReal, "authored-workspaces")}${sep}`),
		);
		const workspaceCheck = await invoke<{ id: string; changed: boolean; snapshotId: string }>("authored_state_check_workspace", {
			input: { threadId: thread.id, workspaceId: workspace.id },
		});
		check("new workspace is clean", workspaceCheck.id === workspace.id && !workspaceCheck.changed);
		const workspaceCommit = await invoke<{ revisionId: string; changed: boolean }>(
			"authored_state_commit_workspace",
			{
				input: {
					threadId: thread.id,
					workspaceId: workspace.id,
					expectedHeadRevisionId: workspace.headRevisionId,
					expectedSnapshotId: workspaceCheck.snapshotId,
					operationId: crypto.randomUUID(),
					message: "Smoke-check authored workspace",
				},
			},
		);
		check("clean workspace commit is idempotent", !workspaceCommit.changed);
		const merged = await invoke<{ status: string }>("authored_state_merge_workspace", {
			input: {
				threadId: thread.id,
				workspaceId: workspace.id,
				expectedHeadRevisionId: workspaceCommit.revisionId,
				operationId: crypto.randomUUID(),
			},
		});
		check("clean workspace merges", merged.status === "merged");
		await invoke("authored_state_remove_workspace", {
			input: { threadId: thread.id, workspaceId: workspace.id },
		});

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
	await section("revision-backed score mutations", async () => {
		if (!hasRealDb) {
			record("score mutations", "skip", "no real luma.db");
			return;
		}
		type Clip = {
			id: string;
			scoreId: string;
			patternId: string;
			startTime: number;
			endTime: number;
			zIndex: number;
			blendMode: string;
			args: Record<string, unknown>;
		};
		let picked:
			| {
					trackId: string;
					venueId: string;
					scoreId: string;
					source: Clip;
					unusedZ: number;
			  }
			| undefined;
		const tracks = await invoke<{ id: string }[]>("list_tracks_enriched", {});
		const venues = await invoke<{ id: string }[]>("list_venues", {});
		for (const track of tracks.slice(0, 80)) {
			for (const venue of venues) {
				const scores = await invoke<{ id: string }[]>("list_scores_for_track", {
					trackId: track.id,
					venueId: venue.id,
				});
				for (const score of scores) {
					const clips = await invoke<Clip[]>("list_track_scores", { scoreId: score.id });
					if (clips[0]) {
						picked = {
							trackId: track.id,
							venueId: venue.id,
							scoreId: score.id,
							source: clips[0],
							unusedZ: Math.max(...clips.map((clip) => clip.zIndex)) + 1,
						};
						break;
					}
				}
				if (picked) break;
			}
			if (picked) break;
		}
		if (!picked) {
			record("score mutations", "skip", "no score with an existing clip");
			return;
		}
		console.log(`  using track=${picked.trackId} venue=${picked.venueId} score=${picked.scoreId}`);

		const thread = await invoke<{ id: string }>("agent_thread_create", {
			input: {
				requestId: crypto.randomUUID(),
				agentKind: "track_copilot",
				subjectKind: "track",
				subjectId: picked.trackId,
				venueId: picked.venueId,
				scoreId: picked.scoreId,
				title: "score mutation smoke",
			},
		});
		const beforePage = await invoke<AuthoredHistoryPage>("authored_state_list_history", {
			threadId: thread.id,
			cursor: null,
			limit: 20,
		});
		const before = beforePage.entries;
		const createResult = await invoke<TrackEditResult>("create_track_score", {
			payload: {
				requestId: crypto.randomUUID(),
				scoreId: picked.scoreId,
				trackId: picked.trackId,
				patternId: picked.source.patternId,
				startTime: picked.source.startTime,
				endTime: picked.source.endTime,
				zIndex: picked.unusedZ,
				blendMode: picked.source.blendMode,
				args: picked.source.args,
			},
		});
		const created = createResult.clips.find(
			(clip) => clip.id === createResult.createdClipId,
		);
		if (!created) throw new Error("create_track_score did not return its created clip");
		check(
			"UI-style create returns the persisted clip",
			createResult.added === 1 && createResult.createdClipId === created.id,
		);
		const updatedZ = created.zIndex + 1;
		await invoke<TrackEditResult>("update_track_score", {
			payload: {
				operationId: crypto.randomUUID(),
				scoreId: picked.scoreId,
				trackId: picked.trackId,
				id: created.id,
				zIndex: updatedZ,
			},
		});
		const updated = (await invoke<Clip[]>("list_track_scores", { scoreId: picked.scoreId })).find(
			(clip) => clip.id === created.id,
		);
		check("UI-style update advances the live projection", updated?.zIndex === updatedZ);
		await invoke<TrackEditResult>("delete_track_score", {
			payload: {
				operationId: crypto.randomUUID(),
				scoreId: picked.scoreId,
				trackId: picked.trackId,
				id: created.id,
			},
		});
		const afterDelete = await invoke<Clip[]>("list_track_scores", { scoreId: picked.scoreId });
		check("UI-style delete advances the live projection", !afterDelete.some((clip) => clip.id === created.id));
		const afterPage = await invoke<AuthoredHistoryPage>("authored_state_list_history", {
			threadId: thread.id,
			cursor: null,
			limit: 20,
		});
		const after = afterPage.entries;
		check(
			"create, update, and delete each remain in revision history",
			after.length === before.length + 3 && new Set(after.map((entry) => entry.revisionId)).size === after.length,
			`history ${before.length} → ${after.length}`,
		);
		await invoke("agent_thread_delete", { threadId: thread.id });
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
			await invoke("get_pattern_graph_document", {});
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
		const patterns = await invoke<{ id: string; name: string; uid: string | null }[]>(
			"list_patterns",
			{},
		);
		let graph: { nodes: unknown[]; edges: unknown[] } | undefined;
		let patternName = "";
		let patternId = "";
		for (const p of patterns) {
			if (p.uid !== null) continue;
			const document = await invoke<{ graph: { nodes?: unknown[]; edges?: unknown[] } }>(
				"get_pattern_graph_document",
				{ id: p.id, implementationId: null },
			);
			const parsed = document.graph;
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
