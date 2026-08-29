/**
 * Pick the light-arena track pool out of the real Luma library.
 *
 *   bun run scripts/arena/select_tracks.ts --dry-run
 *   bun run scripts/arena/select_tracks.ts --size 200 --seed 1
 *
 * Selection is genre mix plus boring filters — deliberately not a structure or
 * energy analysis. A track's genre comes from `track_genres`: Discogs-EffNet
 * emits a sparse per-bar top-K, and the track's mix is the mean probability per
 * label across bars (labels absent from a bar count as zero, which is what the
 * sparse encoding means). Primary genre is the argmax of that mix.
 *
 * `labels_json` is *per track* — the worker compacts the 400-label taxonomy to
 * the styles a track actually uses and remaps every index into that compact
 * list, so label indices are meaningless across rows. Always resolve through
 * the row's own labels.
 *
 * The library is opened read-only-by-convention (only SELECTs). `file:?mode=ro`
 * URIs fail against this DB, so pass the plain path; `--copy` snapshots it
 * first if the app is running.
 */

import { Database } from "bun:sqlite";
import { createHash } from "node:crypto";
import { copyFileSync, existsSync, mkdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join } from "node:path";

const DEFAULT_DB = join(homedir(), "Library/Application Support/com.luma.luma/luma.db");
const DEFAULT_ENGINE_DB = join(homedir(), "Music/Engine Library/Database2/m.db");
const REPO_ROOT = join(dirname(Bun.fileURLToPath(import.meta.url)), "../..");

/** Playlists the arena pool was imported from; used only to label a row. */
const ENGINE_PLAYLISTS = [
	"Techno/Aurora",
	"Techno/Hard",
	"UKG",
	"Civilian Wubs",
	"Drum and Bass",
	"Disco House",
	"Upbeat Groove",
];

type Options = {
	db: string;
	engineDb: string;
	outDir: string;
	size: number;
	holdout: number;
	seed: number;
	minPerGenre: number;
	minDuration: number;
	maxDuration: number;
	perArtist: number;
	dryRun: boolean;
	copy: boolean;
};

function parseArgs(argv: string[]): Options {
	const o: Options = {
		db: DEFAULT_DB,
		engineDb: DEFAULT_ENGINE_DB,
		outDir: join(REPO_ROOT, "arena"),
		size: 200,
		holdout: 0.2,
		seed: 1,
		minPerGenre: 8,
		minDuration: 120,
		maxDuration: 480,
		perArtist: 2,
		dryRun: false,
		copy: false,
	};
	for (let i = 0; i < argv.length; i++) {
		const arg = argv[i];
		const value = () => {
			const v = argv[++i];
			if (v === undefined) throw new Error(`${arg} requires a value`);
			return v;
		};
		switch (arg) {
			case "--db": o.db = value(); break;
			case "--engine-db": o.engineDb = value(); break;
			case "--out-dir": o.outDir = value(); break;
			case "--size": o.size = Number(value()); break;
			case "--holdout": o.holdout = Number(value()); break;
			case "--seed": o.seed = Number(value()); break;
			case "--min-per-genre": o.minPerGenre = Number(value()); break;
			case "--min-duration": o.minDuration = Number(value()); break;
			case "--max-duration": o.maxDuration = Number(value()); break;
			case "--per-artist": o.perArtist = Number(value()); break;
			case "--dry-run": o.dryRun = true; break;
			case "--copy": o.copy = true; break;
			case "--help":
				console.log("usage: select_tracks.ts [--size N] [--holdout F] [--seed N] [--min-per-genre N] [--dry-run] [--copy] [--db PATH] [--out-dir DIR]");
				process.exit(0);
				break;
			default:
				throw new Error(`unknown argument ${arg}`);
		}
	}
	return o;
}

/* ---------------------------------------------------------------- rng ----- */

/** mulberry32 — small, deterministic, good enough for a shuffle. */
function rng(seed: number): () => number {
	let a = seed >>> 0;
	return () => {
		a = (a + 0x6d2b79f5) >>> 0;
		let t = Math.imul(a ^ (a >>> 15), 1 | a);
		t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
		return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
	};
}

function shuffled<T>(items: T[], next: () => number): T[] {
	const out = items.slice();
	for (let i = out.length - 1; i > 0; i--) {
		const j = Math.floor(next() * (i + 1));
		[out[i], out[j]] = [out[j], out[i]];
	}
	return out;
}

/* --------------------------------------------------------- normalizing ---- */

const FEAT = /\s*(?:\(|\[)?\s*\b(?:feat|ft|featuring|with|vs|versus)\b\.?\s.*$/i;
const REMIX_TAIL = /\s*[-–]\s*[^-–]*\b(?:remix|rmx|edit|mix|version|bootleg|rework|dub|vip|extended|radio)\b.*$/i;
const BRACKETED = /\s*[([{][^)\]}]*[)\]}]/g;

/** Primary artist key: first credited name, lowercased, punctuation stripped. */
function artistKey(artist: string): string {
	const first = artist
		.replace(FEAT, "")
		.split(/,|\/|&|\bx\b|\bvs\b/i)[0]
		.toLowerCase()
		.replace(/[^a-z0-9]+/g, " ")
		.trim();
	return first;
}

/**
 * Title key for near-duplicate collapse: remixes, edits and extended cuts of
 * one title fold onto the same key. Bracketed suffixes and a trailing
 * `- Foo Remix` are dropped, as are feature credits.
 */
function titleKey(title: string): string {
	return title
		.replace(BRACKETED, "")
		.replace(REMIX_TAIL, "")
		.replace(FEAT, "")
		.toLowerCase()
		.replace(/[^a-z0-9]+/g, " ")
		.trim();
}

/**
 * Many rows carry the artist inside the title (`Artist - Title`) and an empty
 * artist column — Engine's export of a filename-derived tag. Split it back out
 * so the per-artist cap sees something.
 */
function splitArtistTitle(artist: string, title: string): { artist: string; title: string } {
	if (artist.trim()) return { artist: artist.trim(), title: title.trim() };
	const dash = title.indexOf(" - ");
	if (dash > 0) return { artist: title.slice(0, dash).trim(), title: title.slice(dash + 3).trim() };
	return { artist: "", title: title.trim() };
}

/* ------------------------------------------------------------- library ---- */

type Row = {
	id: string;
	title: string;
	artist: string;
	duration: number;
	bpm: number | null;
	sourceId: string | null;
	genresJson: string;
	labelsJson: string;
};

type Track = {
	id: string;
	title: string;
	artist: string;
	duration: number;
	bpm: number | null;
	playlist: string | null;
	mix: [string, number][]; // descending
	primary: string;
};

/** Mean probability per label across bars; sparse absences count as zero. */
function genreMix(genresJson: string, labelsJson: string): [string, number][] {
	const labels: string[] = JSON.parse(labelsJson);
	const parsed = JSON.parse(genresJson) as { bars: { top: [number, number][] }[] };
	const bars = parsed.bars ?? [];
	if (bars.length === 0) return [];
	const sums = new Float64Array(labels.length);
	for (const bar of bars) for (const [idx, p] of bar.top) sums[idx] += p;
	return labels
		.map((label, i): [string, number] => [label, sums[i] / bars.length])
		.filter(([, p]) => p > 0)
		.sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]));
}

function loadPlaylists(engineDb: string): Map<string, string> {
	const byTrack = new Map<string, string>();
	if (!existsSync(engineDb)) return byTrack;
	const db = new Database(engineDb, { readonly: true });
	const lists = db
		.query<{ id: number; title: string; parentListId: number | null }, []>(
			"SELECT id, title, parentListId FROM Playlist",
		)
		.all();
	const byId = new Map(lists.map((l) => [l.id, l]));
	const path = (l: (typeof lists)[number]): string => {
		const parent = l.parentListId ? byId.get(l.parentListId) : undefined;
		return parent && parent.id !== l.id ? `${path(parent)}/${l.title}` : l.title;
	};
	const wanted = new Map<number, string>();
	for (const l of lists) {
		const p = path(l);
		if (ENGINE_PLAYLISTS.includes(p)) wanted.set(l.id, p);
	}
	const entries = db
		.query<{ listId: number; trackId: number; databaseUuid: string }, []>(
			"SELECT listId, trackId, databaseUuid FROM PlaylistEntity",
		)
		.all();
	// Deterministic when a track sits in two arena playlists: first by name.
	for (const e of entries.sort((a, b) => a.listId - b.listId)) {
		const name = wanted.get(e.listId);
		if (!name) continue;
		const key = `${e.databaseUuid}:${e.trackId}`;
		if (!byTrack.has(key)) byTrack.set(key, name);
	}
	db.close();
	return byTrack;
}

function loadTracks(opts: Options): { all: Track[]; totals: Record<string, number> } {
	const db = new Database(opts.db, { readonly: true });
	const rows = db
		.query<Row, []>(`
			SELECT t.id                        AS id,
			       COALESCE(t.title, '')       AS title,
			       COALESCE(t.artist, '')      AS artist,
			       t.duration_seconds          AS duration,
			       b.bpm                       AS bpm,
			       t.source_id                 AS sourceId,
			       g.genres_json               AS genresJson,
			       g.labels_json               AS labelsJson
			  FROM tracks t
			  JOIN track_genres g               ON g.track_id = t.id
			  JOIN track_beats  b               ON b.track_id = t.id
			  JOIN track_bar_classifications c  ON c.track_id = t.id
			 WHERE t.duration_seconds IS NOT NULL
			   AND EXISTS (SELECT 1 FROM track_stems s WHERE s.track_id = t.id)
			 ORDER BY t.id
		`)
		.all();
	const totalTracks = db.query<{ n: number }, []>("SELECT COUNT(*) AS n FROM tracks").get()!.n;
	db.close();

	const playlists = loadPlaylists(opts.engineDb);
	const all: Track[] = [];
	for (const r of rows) {
		const { artist, title } = splitArtistTitle(r.artist, r.title);
		const mix = genreMix(r.genresJson, r.labelsJson);
		if (mix.length === 0) continue;
		all.push({
			id: r.id,
			title,
			artist,
			duration: r.duration,
			bpm: r.bpm,
			playlist: r.sourceId ? (playlists.get(r.sourceId) ?? null) : null,
			mix,
			primary: mix[0][0],
		});
	}
	return { all, totals: { tracks: totalTracks, analyzed: all.length } };
}

/* ------------------------------------------------------------- filters ---- */

type Eligible = { kept: Track[]; dropped: Record<string, number> };

function filter(all: Track[], opts: Options): Eligible {
	const dropped: Record<string, number> = { duration: 0, duplicate_title: 0, artist_cap: 0 };
	const seenTitle = new Set<string>();
	const perArtist = new Map<string, number>();
	const kept: Track[] = [];
	// `all` is ordered by track id, so which member of a duplicate pair survives
	// is stable across runs.
	for (const t of all) {
		if (t.duration < opts.minDuration || t.duration > opts.maxDuration) {
			dropped.duration++;
			continue;
		}
		const tkey = titleKey(t.title);
		if (tkey && seenTitle.has(tkey)) {
			dropped.duplicate_title++;
			continue;
		}
		const akey = artistKey(t.artist);
		if (akey) {
			const n = perArtist.get(akey) ?? 0;
			if (n >= opts.perArtist) {
				dropped.artist_cap++;
				continue;
			}
			perArtist.set(akey, n + 1);
		}
		if (tkey) seenTitle.add(tkey);
		kept.push(t);
	}
	return { kept, dropped };
}

/* -------------------------------------------------------------- quotas ---- */

const OTHER = "other";

type Bucket = { name: string; tracks: Track[] };

function bucketize(kept: Track[], minPerGenre: number): Bucket[] {
	const byGenre = new Map<string, Track[]>();
	for (const t of kept) {
		const list = byGenre.get(t.primary) ?? [];
		list.push(t);
		byGenre.set(t.primary, list);
	}
	const buckets: Bucket[] = [];
	const other: Track[] = [];
	for (const [name, tracks] of [...byGenre].sort((a, b) => a[0].localeCompare(b[0]))) {
		if (tracks.length >= minPerGenre) buckets.push({ name, tracks });
		else other.push(...tracks);
	}
	if (other.length > 0) {
		other.sort((a, b) => a.id.localeCompare(b.id));
		buckets.push({ name: OTHER, tracks: other });
	}
	return buckets;
}

/**
 * Equal share per bucket, capped by availability, with the leftover from
 * undersized buckets handed to the largest remaining ones (ties by name) one
 * seat at a time.
 */
function quotas(buckets: Bucket[], size: number): Map<string, number> {
	const quota = new Map<string, number>();
	if (buckets.length === 0) return quota;
	const share = Math.floor(size / buckets.length);
	for (const b of buckets) quota.set(b.name, Math.min(share, b.tracks.length));
	let left = size - [...quota.values()].reduce((a, b) => a + b, 0);
	while (left > 0) {
		const room = buckets
			.filter((b) => b.tracks.length > quota.get(b.name)!)
			.sort((a, b) => {
				const ra = a.tracks.length - quota.get(a.name)!;
				const rb = b.tracks.length - quota.get(b.name)!;
				return rb - ra || a.name.localeCompare(b.name);
			});
		if (room.length === 0) break;
		for (const b of room) {
			if (left === 0) break;
			quota.set(b.name, quota.get(b.name)! + 1);
			left--;
		}
	}
	return quota;
}

/* -------------------------------------------------------------- output ---- */

/**
 * Discogs labels are `Parent---Style`; the style alone reads better, but two
 * parents can share one style name (`Disco` lives under both Electronic and
 * Funk / Soul), so a colliding style keeps its parent.
 */
function displayNames(labels: Iterable<string>): (label: string) => string {
	const style = (l: string) => (l.includes("---") ? l.split("---")[1] : l);
	const seen = new Map<string, Set<string>>();
	for (const l of labels) {
		const s = style(l);
		(seen.get(s) ?? seen.set(s, new Set()).get(s)!).add(l);
	}
	return (label: string) => {
		const s = style(label);
		return (seen.get(s)?.size ?? 0) > 1 ? label.replace("---", " / ") : s;
	};
}

function csvCell(v: unknown): string {
	const s = v === null || v === undefined ? "" : String(v);
	return /[",\n]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
}

function hashFile(path: string): string {
	return createHash("sha256").update(readFileSync(path)).digest("hex");
}

function pad(s: string, n: number): string {
	return s.length >= n ? s.slice(0, n) : s + " ".repeat(n - s.length);
}

function main() {
	const opts = parseArgs(process.argv.slice(2));
	let dbPath = opts.db;
	if (opts.copy) {
		const scratch = join(opts.outDir, ".snapshot.db");
		mkdirSync(opts.outDir, { recursive: true });
		copyFileSync(opts.db, scratch);
		dbPath = scratch;
	}
	const { all, totals } = loadTracks({ ...opts, db: dbPath });
	const label = displayNames(new Set(all.flatMap((t) => t.mix.map(([l]) => l))));

	// Pool histogram — before any filtering, so the mix we started from is
	// visible next to what survived.
	const hist = new Map<string, number>();
	for (const t of all) hist.set(t.primary, (hist.get(t.primary) ?? 0) + 1);
	console.log(`pool: ${totals.tracks} tracks, ${totals.analyzed} fully analyzed with genres\n`);
	console.log("primary-genre histogram (analyzed pool):");
	for (const [g, n] of [...hist].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))) {
		console.log(`  ${pad(label(g), 30)} ${String(n).padStart(4)}  ${"#".repeat(n)}`);
	}

	const { kept, dropped } = filter(all, opts);
	console.log(
		`\nfilters: ${kept.length} eligible (dropped ${dropped.duration} duration, ` +
			`${dropped.duplicate_title} duplicate title, ${dropped.artist_cap} artist cap)`,
	);

	const buckets = bucketize(kept, opts.minPerGenre);
	const quota = quotas(buckets, opts.size);
	const next = rng(opts.seed);
	const selected: (Track & { bucket: string; holdout: boolean })[] = [];
	const perBucket: Record<string, { eligible: number; selected: number; holdout: number }> = {};
	for (const b of buckets) {
		const picked = shuffled(b.tracks, next).slice(0, quota.get(b.name)!);
		const holdoutN = Math.round(picked.length * opts.holdout);
		picked.forEach((t, i) => {
			selected.push({ ...t, bucket: b.name, holdout: i >= picked.length - holdoutN });
		});
		perBucket[b.name] = {
			eligible: b.tracks.length,
			selected: picked.length,
			holdout: holdoutN,
		};
	}
	selected.sort((a, b) => a.bucket.localeCompare(b.bucket) || a.id.localeCompare(b.id));

	console.log(`\n${pad("genre", 32)}${pad("eligible", 10)}${pad("selected", 10)}holdout`);
	for (const b of buckets) {
		const s = perBucket[b.name];
		console.log(
			`${pad(label(b.name), 32)}${pad(String(s.eligible), 10)}${pad(String(s.selected), 10)}${s.holdout}`,
		);
	}
	const totalSel = selected.length;
	const totalHold = selected.filter((t) => t.holdout).length;
	console.log(`${pad("TOTAL", 32)}${pad(String(kept.length), 10)}${pad(String(totalSel), 10)}${totalHold}`);

	if (opts.dryRun) {
		console.log("\n--dry-run: nothing written");
		return;
	}

	mkdirSync(opts.outDir, { recursive: true });
	const header = [
		"track_id", "title", "artist", "duration_seconds", "bpm",
		"primary_genre", "genre_1", "genre_1_p", "genre_2", "genre_2_p", "genre_3", "genre_3_p",
		"playlist", "holdout",
	];
	const lines = [header.join(",")];
	for (const t of selected) {
		const top3 = t.mix.slice(0, 3);
		lines.push(
			[
				t.id, t.title, t.artist, t.duration.toFixed(3), t.bpm === null ? "" : t.bpm.toFixed(2),
				label(t.bucket === OTHER ? t.primary : t.bucket),
				...[0, 1, 2].flatMap((i) => [
					top3[i] ? label(top3[i][0]) : "",
					top3[i] ? top3[i][1].toFixed(4) : "",
				]),
				t.playlist ?? "",
				t.holdout ? 1 : 0,
			].map(csvCell).join(","),
		);
	}
	const csvPath = join(opts.outDir, "tracks.csv");
	writeFileSync(csvPath, `${lines.join("\n")}\n`);

	const stat = statSync(opts.db);
	const manifest = {
		params: {
			size: opts.size,
			holdout: opts.holdout,
			seed: opts.seed,
			minPerGenre: opts.minPerGenre,
			durationSeconds: [opts.minDuration, opts.maxDuration],
			tracksPerArtist: opts.perArtist,
		},
		library: {
			path: opts.db,
			sha256: hashFile(dbPath),
			mtime: stat.mtime.toISOString(),
			tracks: totals.tracks,
			analyzed: totals.analyzed,
			eligible: kept.length,
		},
		dropped,
		genres: Object.fromEntries(
			buckets.map((b) => [b.name, { ...perBucket[b.name], quota: quota.get(b.name)! }]),
		),
		selected: totalSel,
		holdout: totalHold,
	};
	const manifestPath = join(opts.outDir, "manifest.json");
	writeFileSync(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);
	console.log(`\nwrote ${csvPath}\nwrote ${manifestPath}`);
}

main();
