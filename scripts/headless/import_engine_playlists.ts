/**
 * Import whole Engine DJ playlists into a Luma library, headlessly, and stay
 * up until every imported track is analyzed.
 *
 * The app's import command (`engine_dj_import_tracks`) does two things: a fast
 * synchronous copy-and-insert per track, then a *background* analysis task
 * spawned onto the host's runtime. Headless, that runtime is the
 * `agent_harness` process — so the harness must outlive the command by however
 * long the DAG takes (beats, stems, MERT, roots, drum onsets, bar classifier,
 * genre). That waiting, and the progress reporting around it, is what this
 * script is for.
 *
 *   bun run scripts/headless/import_engine_playlists.ts --dry-run
 *   bun run scripts/headless/import_engine_playlists.ts
 *
 * Library selection: **the real app config dir by default** — the harness's
 * own `StorageRoot::from_env_default()`. `--config-dir` (or `LUMA_CONFIG_DIR`)
 * points it at a scratch copy instead.
 *
 * Idempotent and resumable. Already-imported tracks are filtered out before
 * the import call (dedupe on `source_id` = `<databaseUuid>:<engineTrackId>`,
 * the same key `engine_dj_fast_import` dedupes on), and `--reprocess` re-arms
 * analysis for tracks that were imported by an earlier run whose harness died
 * before the DAG finished — a headless host runs no startup reconcile, so
 * nothing else would ever pick them up.
 *
 * Tracks go in chunks of `--chunk` (default 8), each chunk analyzed before the
 * next is imported — that is what makes progress reportable and a killed run
 * resumable. Concurrency inside a chunk is the backend's own business: both
 * branches of a batch bound themselves by `analysis_worker_count()`.
 */

import { Database } from "bun:sqlite";
import { existsSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

import { startHarness } from "./shim";

/** Titles to import, in order. `Parent/Child` disambiguates a nested playlist. */
const DEFAULT_PLAYLISTS = [
	"Techno/Aurora",
	"Techno/Hard",
	"UKG",
	"Civilian Wubs",
	"Drum and Bass",
	"Disco House",
	"Upbeat Groove",
];

const DEFAULT_LIBRARY = join(homedir(), "Music/Engine Library");

/** The `TrackBrowserRow` flags that mean "the DAG finished for this track". */
const ARTIFACT_FLAGS = [
	["has_beats", "hasBeats", "track_beats"],
	["has_stems", "hasStems", "track_stems"],
	["has_roots", "hasRoots", "track_roots"],
	["has_drum_onsets", "hasDrumOnsets", "track_drum_onsets"],
	["has_bar_classifications", "hasBarClassifications", "track_bar_classifications"],
	["has_genres", "hasGenres", "track_genres"],
] as const;

type EnginePlaylist = { id: number; title: string; parentId: number | null; trackCount: number };
type EngineTrack = { id: number; title: string | null; artist: string | null; filename: string };
type TrackSummary = { id: string; sourceType: string | null; sourceId: string | null };
type BrowserRow = { id: string } & Record<string, unknown>;
type ImportResult = {
	importId: string;
	tracks: { id: string }[];
	failures: { sourceId: string; message: string }[];
};

type Options = {
	dryRun: boolean;
	configDir?: string;
	library: string;
	playlists: string[];
	pollSeconds: number;
	analysisTimeoutMinutes: number;
	reprocess: boolean;
	skipAnalysis: boolean;
	chunkSize: number;
};

function parseArgs(argv: string[]): Options {
	const opts: Options = {
		dryRun: false,
		configDir: process.env.LUMA_CONFIG_DIR,
		library: DEFAULT_LIBRARY,
		playlists: [],
		pollSeconds: 30,
		analysisTimeoutMinutes: 45,
		reprocess: true,
		skipAnalysis: false,
		chunkSize: 8,
	};
	for (let i = 0; i < argv.length; i++) {
		const arg = argv[i];
		const value = () => {
			const v = argv[++i];
			if (v === undefined) throw new Error(`${arg} requires a value`);
			return v;
		};
		switch (arg) {
			case "--dry-run":
				opts.dryRun = true;
				break;
			case "--config-dir":
				opts.configDir = value();
				break;
			case "--library":
				opts.library = value();
				break;
			case "--playlist":
				opts.playlists.push(value());
				break;
			case "--poll":
				opts.pollSeconds = Number(value());
				break;
			case "--analysis-timeout":
				opts.analysisTimeoutMinutes = Number(value());
				break;
			case "--no-reprocess":
				opts.reprocess = false;
				break;
			case "--chunk":
				opts.chunkSize = Math.max(1, Number(value()));
				break;
			case "--no-analysis":
				opts.skipAnalysis = true;
				break;
			case "--help":
			case "-h":
				console.log(
					[
						"import_engine_playlists.ts [options]",
						"  --dry-run                 resolve and count, import nothing",
						"  --config-dir <path>       Luma config dir (default: the real app one)",
						"  --library <path>          Engine Library root",
						"  --playlist <title>        repeatable; `Parent/Child` for a nested one",
						"  --no-reprocess            do not re-arm analysis for already-imported tracks",
						"  --chunk <n>               tracks per import/analysis batch (default 8)",
						"  --no-analysis             import and exit without waiting for the DAG",
						"  --poll <seconds>          analysis poll interval (default 30)",
						"  --analysis-timeout <min>  give up on a stalled batch (default 45)",
					].join("\n"),
				);
				process.exit(0);
				break;
			default:
				throw new Error(`unknown flag \`${arg}\``);
		}
	}
	if (opts.playlists.length === 0) opts.playlists = [...DEFAULT_PLAYLISTS];
	return opts;
}

/** Where the harness will put `luma.db`, mirroring `StorageRoot::from_env_default`. */
function resolveConfigDir(opts: Options): string {
	if (opts.configDir) return opts.configDir;
	return join(homedir(), "Library/Application Support/com.luma.luma");
}

/**
 * Resolve a `Parent/Child` (or bare) title against the flat playlist list.
 * Throws rather than guessing: importing the wrong 90 tracks is worse than
 * stopping.
 */
function resolvePlaylist(all: EnginePlaylist[], spec: string): EnginePlaylist {
	const parts = spec.split("/").map((p) => p.trim());
	const leaf = parts[parts.length - 1];
	const byId = new Map(all.map((p) => [p.id, p]));
	let matches = all.filter((p) => p.title === leaf);
	for (let depth = parts.length - 2; depth >= 0; depth--) {
		const wanted = parts[depth];
		matches = matches.filter((p) => {
			const parent = p.parentId == null ? null : byId.get(p.parentId);
			return parent?.title === wanted;
		});
	}
	if (matches.length === 0) throw new Error(`no Engine DJ playlist matches \`${spec}\``);
	if (matches.length > 1) {
		throw new Error(
			`\`${spec}\` is ambiguous (ids ${matches.map((m) => m.id).join(", ")}) — qualify it as Parent/Child`,
		);
	}
	return matches[0];
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

function fmtDuration(ms: number): string {
	const s = Math.round(ms / 1000);
	const h = Math.floor(s / 3600);
	const m = Math.floor((s % 3600) / 60);
	return h > 0 ? `${h}h${String(m).padStart(2, "0")}m` : `${m}m${String(s % 60).padStart(2, "0")}s`;
}

/**
 * Read-only view of the live `luma.db` for the two things the command surface
 * does not expose: `preprocessing_failures` (local-only, no command) and the
 * waveform table (no `TrackBrowserRow` flag).
 */
class LibraryProbe {
	private db: Database | null;

	constructor(configDir: string) {
		const path = join(configDir, "luma.db");
		this.db = existsSync(path) ? new Database(path, { readonly: true }) : null;
	}

	/** `track_id -> [preprocessor, error]` for the given tracks. */
	failures(trackIds: string[]): Map<string, { preprocessor: string; error: string }[]> {
		const out = new Map<string, { preprocessor: string; error: string }[]>();
		if (!this.db || trackIds.length === 0) return out;
		const holes = trackIds.map(() => "?").join(",");
		const rows = this.db
			.query<
				{ track_id: string; preprocessor: string; last_error: string | null },
				string[]
			>(
				`SELECT track_id, preprocessor, last_error FROM preprocessing_failures
				 WHERE track_id IN (${holes})`,
			)
			.all(...trackIds);
		for (const row of rows) {
			const list = out.get(row.track_id) ?? [];
			list.push({ preprocessor: row.preprocessor, error: row.last_error ?? "" });
			out.set(row.track_id, list);
		}
		return out;
	}

	waveformCount(trackIds: string[]): number {
		if (!this.db || trackIds.length === 0) return 0;
		const holes = trackIds.map(() => "?").join(",");
		const row = this.db
			.query<{ n: number }, string[]>(
				`SELECT COUNT(DISTINCT track_id) AS n FROM track_waveforms WHERE track_id IN (${holes})`,
			)
			.get(...trackIds);
		return row?.n ?? 0;
	}

	close() {
		this.db?.close();
		this.db = null;
	}
}

/** `has_*` is snake in the Rust struct and camel over Tauri's serde; accept both. */
function flagged(row: BrowserRow, snake: string, camel: string): boolean {
	return Boolean(row[camel] ?? row[snake]);
}

function analysisCoverage(rows: Map<string, BrowserRow>, trackIds: string[]) {
	const per = new Map<string, number>();
	let complete = 0;
	for (const [, , table] of ARTIFACT_FLAGS) per.set(table, 0);
	for (const id of trackIds) {
		const row = rows.get(id);
		if (!row) continue;
		let all = true;
		for (const [snake, camel, table] of ARTIFACT_FLAGS) {
			if (flagged(row, snake, camel)) per.set(table, (per.get(table) ?? 0) + 1);
			else all = false;
		}
		if (all) complete++;
	}
	return { per, complete };
}

async function main() {
	const opts = parseArgs(process.argv.slice(2));
	const configDir = resolveConfigDir(opts);
	const started = Date.now();

	console.log(`config dir : ${configDir}${opts.configDir ? "" : "  (real app library)"}`);
	console.log(`engine lib : ${opts.library}`);

	const spawned = await startHarness({ configDir });
	const harness = resilient(spawned);
	const probe = new LibraryProbe(configDir);
	let exitCode = 0;

	try {
		// Whoever the app last signed in as owns every row this run writes. The
		// read is offline and tolerates an expired token, exactly as the
		// admission gate the harness already armed does.
		const owner = await harness.invoke<{ id: string; email?: string } | null>("current_account");
		if (!owner?.id) {
			throw new Error(
				"this library has no signed-in account — sign in with the Luma app on this machine first",
			);
		}
		console.log(`owner      : ${owner.id}${owner.email ? `  <${owner.email}>` : ""}`);

		const info = await harness.invoke<{ databaseUuid: string; trackCount: number }>(
			"engine_dj_open_library",
			{ libraryPath: opts.library },
		);
		console.log(`engine uuid: ${info.databaseUuid}  (${info.trackCount} tracks)\n`);

		const all = await harness.invoke<EnginePlaylist[]>("engine_dj_list_playlists", {
			libraryPath: opts.library,
		});

		const existing = new Set(
			(await harness.invoke<TrackSummary[]>("list_tracks"))
				.filter((t) => t.sourceType === "engine_dj" && t.sourceId)
				.map((t) => t.sourceId as string),
		);

		// Resolve every playlist before importing any of them, so a typo fails
		// the run instead of half-importing it.
		type PlannedTrack = EngineTrack & { sourceId: string };
		type Planned = {
			spec: string;
			playlist: EnginePlaylist;
			tracks: PlannedTrack[];
			fresh: PlannedTrack[];
			alreadyIn: number;
			dangling: number;
		};
		const plan: Planned[] = [];
		for (const spec of opts.playlists) {
			const playlist = resolvePlaylist(all, spec);
			const tracks = await harness.invoke<EngineTrack[]>("engine_dj_get_playlist_tracks", {
				libraryPath: opts.library,
				playlistId: playlist.id,
			});
			const withSource = tracks.map((t) => ({
				...t,
				sourceId: `${info.databaseUuid}:${t.id}`,
			}));
			const fresh = withSource.filter((t) => !existing.has(t.sourceId));
			plan.push({
				spec,
				playlist,
				tracks: withSource,
				fresh,
				alreadyIn: withSource.length - fresh.length,
				// `PlaylistEntity` rows whose `trackId` has no `Track` row are
				// dropped by the importer's INNER JOIN, silently.
				dangling: playlist.trackCount - withSource.length,
			});
		}

		console.log(
			`${"playlist".padEnd(22)}${"id".padStart(5)}${"entity".padStart(8)}${"tracks".padStart(8)}${"new".padStart(6)}${"in luma".padStart(9)}${"dangling".padStart(10)}`,
		);
		for (const p of plan) {
			console.log(
				p.spec.padEnd(22) +
					String(p.playlist.id).padStart(5) +
					String(p.playlist.trackCount).padStart(8) +
					String(p.tracks.length).padStart(8) +
					String(p.fresh.length).padStart(6) +
					String(p.alreadyIn).padStart(9) +
					String(p.dangling).padStart(10),
			);
		}
		const totalFresh = plan.reduce((n, p) => n + p.fresh.length, 0);
		console.log(
			`${"TOTAL".padEnd(22)}${"".padStart(5)}${String(plan.reduce((n, p) => n + p.playlist.trackCount, 0)).padStart(8)}${String(plan.reduce((n, p) => n + p.tracks.length, 0)).padStart(8)}${String(totalFresh).padStart(6)}${String(plan.reduce((n, p) => n + p.alreadyIn, 0)).padStart(9)}${String(plan.reduce((n, p) => n + p.dangling, 0)).padStart(10)}\n`,
		);

		if (opts.dryRun) {
			console.log("dry run — nothing imported");
			return;
		}

		const importedAll: string[] = [];
		// Every selected playlist's tracks that exist in the library, whether
		// this run imported them or an earlier one did. The final coverage is
		// reported over these: "are the playlists analyzed" is the question, and
		// a re-run that imported nothing still has to answer it.
		const ownedAll: string[] = [];
		const failuresAll: { playlist: string; sourceId: string; message: string }[] = [];

		for (const p of plan) {
			const label = `[${p.spec}]`;

			// Import in chunks, each one analyzed before the next is imported.
			// `run_waveform_jobs` fans one task out per track with no bound and
			// `luma.db` has 16 pooled connections, so a 65-track batch starves the
			// pool and the artifact writes in flight time out instead of landing.
			const chunks = chunk(p.fresh, opts.chunkSize);
			let importedHere = 0;
			for (const [i, part] of chunks.entries()) {
				const tag = `${label} ${i + 1}/${chunks.length}`;
				console.log(`${tag} importing ${part.length} track(s)…`);
				const result = await harness.invoke<ImportResult>("engine_dj_import_tracks", {
					libraryPath: opts.library,
					trackIds: part.map((t) => t.id),
				});
				const batch = result.tracks.map((t) => t.id);
				for (const f of result.failures) {
					failuresAll.push({ playlist: p.spec, sourceId: f.sourceId, message: f.message });
					console.log(`${tag} import failed ${f.sourceId}: ${f.message}`);
				}
				for (const t of part) existing.add(t.sourceId);
				importedHere += batch.length;
				importedAll.push(...batch);
				console.log(`${tag} imported ${batch.length}, failed ${result.failures.length}`);
				if (batch.length > 0 && !opts.skipAnalysis) {
					await waitForAnalysis(harness, probe, batch, opts, tag);
				}
			}
			if (chunks.length === 0) console.log(`${label} nothing new to import`);
			else console.log(`${label} imported ${importedHere}/${p.fresh.length}`);

			if (opts.skipAnalysis || !opts.reprocess) continue;

			// Every track this playlist owns whose DAG is still incomplete: ones a
			// previous run imported before its harness died, and ones a chunk above
			// left unfinished. A headless host runs no startup reconcile, so nothing
			// else would ever pick them up.
			const summaries = await harness.invoke<TrackSummary[]>("list_tracks");
			const bySource = new Map(
				summaries.filter((t) => t.sourceId).map((t) => [t.sourceId as string, t.id]),
			);
			const owned = p.tracks
				.map((t) => bySource.get(t.sourceId))
				.filter((id): id is string => Boolean(id));
			for (const id of owned) if (!ownedAll.includes(id)) ownedAll.push(id);
			const rows = await browserRows(harness, owned);
			const pending = owned.filter((id) => {
				const row = rows.get(id);
				return !row || ARTIFACT_FLAGS.some(([snake, camel]) => !flagged(row, snake, camel));
			});
			if (pending.length === 0) continue;
			console.log(`${label} re-arming analysis for ${pending.length} pending track(s)`);
			const requeues = chunk(pending, opts.chunkSize);
			for (const [i, part] of requeues.entries()) {
				const tag = `${label} requeue ${i + 1}/${requeues.length}`;
				for (const id of part) await harness.invoke("reprocess_track", { trackId: id });
				for (const id of part) if (!importedAll.includes(id)) importedAll.push(id);
				await waitForAnalysis(harness, probe, part, opts, tag);
			}
		}

		console.log("");
		console.log(`imported this run: ${importedAll.length}`);
		const covered = ownedAll.length > 0 ? ownedAll : importedAll;
		if (covered.length > 0) await report(harness, probe, covered, failuresAll);
		if (failuresAll.length > 0) exitCode = 1;
		console.log(`\nwall time: ${fmtDuration(Date.now() - started)}`);
	} finally {
		probe.close();
		await spawned.close();
	}
	process.exit(exitCode);
}

/** What every helper here needs from the harness. */
type Api = { invoke: <T>(cmd: string, args?: Record<string, unknown>) => Promise<T> };

/**
 * The harness pipe loses a frame occasionally under load — a mangled request
 * line comes back as `{"id": null, "err": "malformed request JSON"}`, which the
 * shim can attribute to nobody, so the caller's promise never settles and a run
 * hours long stops dead in silence. Bound every call and retry it. Retrying is
 * safe for all of them: the reads are reads, `reprocess_track` is idempotent,
 * and `engine_dj_import_tracks` dedupes on `source_id`.
 */
function resilient(harness: Api, timeoutMs = 10 * 60_000, attempts = 3): Api {
	const invoke = async <T>(cmd: string, args?: Record<string, unknown>): Promise<T> => {
		let last: unknown;
		for (let attempt = 1; attempt <= attempts; attempt++) {
			let timer: ReturnType<typeof setTimeout> | undefined;
			try {
				return await Promise.race([
					harness.invoke<T>(cmd, args),
					new Promise<never>((_, reject) => {
						timer = setTimeout(
							() => reject(new Error(`\`${cmd}\` did not answer in ${timeoutMs / 1000}s`)),
							timeoutMs,
						);
					}),
				]);
			} catch (error) {
				last = error;
				console.log(`[harness] ${cmd} attempt ${attempt}/${attempts} failed: ${String(error)}`);
				if (attempt < attempts) await sleep(5_000);
			} finally {
				if (timer) clearTimeout(timer);
			}
		}
		throw last;
	};
	return { invoke };
}

/**
 * Enriched rows for `trackIds`, retried: the same 16-connection pool the DAG is
 * hammering serves this read, so under load it answers "pool timed out" rather
 * than failing permanently. A poll that propagated that would kill the run — and
 * with it the analysis it was waiting for.
 */
async function browserRows(
	harness: Api,
	trackIds: string[],
	attempts = 5,
): Promise<Map<string, BrowserRow>> {
	const want = new Set(trackIds);
	let last: unknown;
	for (let attempt = 0; attempt < attempts; attempt++) {
		try {
			const rows = await harness.invoke<BrowserRow[]>("list_tracks_enriched", { venueId: null });
			return new Map(rows.filter((r) => want.has(r.id)).map((r) => [r.id, r]));
		} catch (error) {
			last = error;
			console.log(`[poll] enriched read failed (${String(error)}); retrying`);
			await sleep(15_000);
		}
	}
	throw last;
}

function chunk<T>(items: T[], size: number): T[][] {
	const out: T[][] = [];
	for (let i = 0; i < items.length; i += size) out.push(items.slice(i, i + size));
	return out;
}

/**
 * Block until every track in `batch` has all six artifacts, or until the batch
 * stalls: no forward progress for `--analysis-timeout` minutes *and* every
 * incomplete track carries a `preprocessing_failures` row. A stalled batch is
 * reported and skipped, not fatal — the remaining playlists still import.
 */
async function waitForAnalysis(
	harness: Api,
	probe: LibraryProbe,
	batch: string[],
	opts: Options,
	label: string,
): Promise<void> {
	const deadlineMs = opts.analysisTimeoutMinutes * 60_000;
	let best = -1;
	let lastProgress = Date.now();
	for (;;) {
		const rows = await browserRows(harness, batch);
		const { per, complete } = analysisCoverage(rows, batch);
		if (complete > best) {
			best = complete;
			lastProgress = Date.now();
		}
		const detail = [...per.entries()]
			.map(([table, n]) => `${table.replace("track_", "")} ${n}`)
			.join("  ");
		console.log(`${label} analyzed ${complete}/${batch.length}   ${detail}`);
		if (complete === batch.length) return;

		const stalledFor = Date.now() - lastProgress;
		if (stalledFor > deadlineMs) {
			const failures = probe.failures(batch);
			const stuck = batch.filter((id) => {
				const row = rows.get(id);
				return !row || ARTIFACT_FLAGS.some(([s, c]) => !flagged(row, s, c));
			});
			const allFailed = stuck.every((id) => (failures.get(id)?.length ?? 0) > 0);
			console.log(
				`${label} no progress for ${fmtDuration(stalledFor)}; ${stuck.length} incomplete, ` +
					`${allFailed ? "all carry failure rows" : "some carry no failure row"} — moving on`,
			);
			return;
		}
		await sleep(opts.pollSeconds * 1000);
	}
}

async function report(
	harness: Api,
	probe: LibraryProbe,
	trackIds: string[],
	importFailures: { playlist: string; sourceId: string; message: string }[],
) {
	const rows = await browserRows(harness, trackIds);
	const { per, complete } = analysisCoverage(rows, trackIds);
	console.log(`analysis coverage over ${trackIds.length} playlist track(s):`);
	for (const [table, n] of per) console.log(`  ${table.padEnd(28)} ${n}/${trackIds.length}`);
	console.log(`  ${"track_waveforms".padEnd(28)} ${probe.waveformCount(trackIds)}/${trackIds.length}`);
	console.log(`  ${"fully analyzed".padEnd(28)} ${complete}/${trackIds.length}`);

	const failures = probe.failures(trackIds);
	if (failures.size === 0 && importFailures.length === 0) {
		console.log("\nno failures");
		return;
	}
	if (importFailures.length > 0) {
		console.log("\nimport failures:");
		for (const f of importFailures) console.log(`  ${f.playlist} ${f.sourceId}: ${f.message}`);
	}
	if (failures.size > 0) {
		console.log("\npreprocessing_failures:");
		for (const [trackId, list] of failures) {
			const row = rows.get(trackId) as { title?: string } | undefined;
			for (const f of list) {
				console.log(
					`  ${trackId} ${row?.title ?? ""} — ${f.preprocessor}: ${f.error.slice(0, 300)}`,
				);
			}
		}
	}
}

await main();
