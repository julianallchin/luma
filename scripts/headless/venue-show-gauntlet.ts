/** MCP-only builder/show runs in a fresh, retained library. No cloud credentials or library copies. */
import { Database } from "bun:sqlite";
import { execFileSync, spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { copyFileSync, createWriteStream, existsSync, mkdirSync, mkdtempSync, readFileSync, statSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";
import { mcpArgs, mcpBinary, REAL_CACHE_DIR } from "./mcp-client";

const repo = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const principal = "gauntlet-fixture-owner";
type Manifest = { version: 1; configDir: string; venueId: string; trackId: string; cacheDir: string };
const args = process.argv.slice(2);
const phase = args.shift();
function option(name: string): string | undefined {
    const index = args.indexOf(name);
    if (index < 0) return undefined;
    const value = args[index + 1];
    if (!value || value.startsWith("--")) throw new Error(`${name} requires a value`);
    args.splice(index, 2);
    return value;
}
const runOption = option("--run");
const model = option("--model");
const customPrompt = option("--prompt-file");
const cacheOption = option("--cache-dir");
if (args.length || !["prepare", "venue", "show", "inspect"].includes(phase ?? "")) {
    throw new Error("Usage: bun scripts/headless/venue-show-gauntlet.ts prepare [--cache-dir PATH] | venue|show|inspect --run DIR [--model MODEL] [--prompt-file FILE]");
}

async function command(binary: string, argv: string[], cwd: string, prefix: string, agent = true): Promise<void> {
    const stdout = createWriteStream(`${prefix}.jsonl`);
    const stderr = createWriteStream(`${prefix}.stderr.log`);
    const child = spawn(binary, argv, { cwd, stdio: ["ignore", "pipe", "pipe"],
        env: { ...process.env, PYTHONDONTWRITEBYTECODE: "1", MPLCONFIGDIR: join(cwd, "matplotlib") } });
    child.stdout.pipe(stdout);
    child.stderr.pipe(stderr);
    const stop = () => child.kill("SIGTERM");
    process.once("SIGINT", stop);
    process.once("SIGTERM", stop);
    const status = await new Promise<number>((yes, no) => {
        child.once("error", no);
        child.once("close", code => yes(code ?? 1));
    });
    process.removeListener("SIGINT", stop);
    process.removeListener("SIGTERM", stop);
    await Promise.all([new Promise<void>(r => stdout.end(r)), new Promise<void>(r => stderr.end(r))]);
    if (status !== 0) throw new Error(`${binary} exited ${status}; inspect ${prefix}.stderr.log`);
    if (!agent) return;
    const events = readFileSync(`${prefix}.jsonl`, "utf8").split("\n").filter(Boolean).map(line => JSON.parse(line));
    const result = events.findLast(event => event.type === "result");
    if (!result || result.is_error || result.subtype !== "success") {
        throw new Error(`Agent did not report success; inspect ${prefix}.jsonl`);
    }
}

async function createVenue(configDir: string, cacheDir: string): Promise<string> {
    const binary = process.env.LUMA_HARNESS_BIN ?? join(repo, "backend/target/debug/agent_harness");
    if (!existsSync(binary)) throw new Error("Build agent_harness first with cargo +1.97.1 build --manifest-path backend/Cargo.toml --bin agent_harness --bin luma-mcp");
    const errors = createWriteStream(join(dirname(configDir), "prepare.stderr.log"));
    const child = spawn(binary, mcpArgs({ configDir, cacheDir, fixturePrincipal: principal }), { cwd: repo, stdio: ["pipe", "pipe", "pipe"] });
    child.stderr.pipe(errors);
    try {
        return await new Promise<string>((yes, no) => {
            const timer = setTimeout(() => no(new Error("Venue bootstrap timed out")), 120_000);
            child.once("error", error => { clearTimeout(timer); no(error); });
            child.once("exit", code => { clearTimeout(timer); no(new Error(`Bootstrap exited ${code}`)); });
            createInterface({ input: child.stdout }).on("line", line => {
                const result = JSON.parse(line);
                if (result.id !== 1) return;
                clearTimeout(timer);
                if (result.err) no(new Error(result.err));
                else yes(result.ok.id);
            });
            child.stdin.write(JSON.stringify({ id: 1, cmd: "create_venue", args: { name: "Gauntlet room", description: "Disposable MCP acceptance fixture" } }) + "\n");
        });
    } finally {
        child.stdin.end();
        if (child.exitCode === null) {
            const timer = setTimeout(() => child.kill(), 5000);
            await new Promise<void>(r => child.once("close", () => r()));
            clearTimeout(timer);
        }
        errors.end();
    }
}

function seedTrack(configDir: string): string {
    // A real PCM file with a deterministic beat, no downloaded audio or analysis service.
    const sampleRate = 8000, seconds = 64, count = sampleRate * seconds;
    const wav = Buffer.alloc(44 + count * 2);
    wav.write("RIFF"); wav.writeUInt32LE(wav.length - 8, 4); wav.write("WAVEfmt ", 8);
    wav.writeUInt32LE(16, 16); wav.writeUInt16LE(1, 20); wav.writeUInt16LE(1, 22);
    wav.writeUInt32LE(sampleRate, 24); wav.writeUInt32LE(sampleRate * 2, 28);
    wav.writeUInt16LE(2, 32); wav.writeUInt16LE(16, 34); wav.write("data", 36); wav.writeUInt32LE(count * 2, 40);
    for (let i = 0; i < count; i++) {
        const t = i / sampleRate, beatTime = t % 0.5;
        wav.writeInt16LE(Math.round(12000 * Math.sin(2 * Math.PI * 90 * t) * Math.exp(-beatTime * 45)), 44 + i * 2);
    }
    const audio = join(configDir, "gauntlet-pulse.wav");
    writeFileSync(audio, wav);
    const id = crypto.randomUUID();
    const db = new Database(join(configDir, "luma.db"));
    try {
        db.transaction(() => {
            db.query("INSERT INTO tracks (id,uid,track_hash,title,duration_seconds,file_path) VALUES (?,?,?,?,?,?)")
                .run(id, principal, "gauntlet-pulse-v1", "Gauntlet pulse 120 BPM", seconds, audio);
            db.query("INSERT INTO track_beats (track_id,uid,beats_json,downbeats_json,bpm,downbeat_offset,beats_per_bar) VALUES (?,?,?,?,120,0,4)")
                .run(id, principal, JSON.stringify(Array.from({ length: 128 }, (_, i) => i / 2)), JSON.stringify(Array.from({ length: 32 }, (_, i) => i * 2)));
        })();
    } finally { db.close(); }
    return id;
}

function inspect(run: string, manifest: Manifest) {
    const db = new Database(join(manifest.configDir, "luma.db"), { readonly: true });
    try {
        const tables = db.query<{ name: string }, []>("SELECT name FROM sqlite_master WHERE type='table' AND (name GLOB 'venue*' OR name GLOB 'fixture*' OR name GLOB 'agent_thread*' OR name IN ('scores','track_scores','patterns','implementations')) ORDER BY name").all();
        const snapshot = Object.fromEntries(tables.map(({ name }) => [name, db.query(`SELECT * FROM "${name.replaceAll('"', '""')}"`).all()]));
        writeFileSync(join(run, "snapshot.json"), JSON.stringify(snapshot, null, 2));
    } finally { db.close(); }
    const metrics = [];
    const trajectory = [];
    for (const stage of ["venue", "show"]) {
        const trace = join(run, stage, "mcp.jsonl");
        if (!existsSync(trace)) continue;
        const requests = new Map<string, any>();
        let legacyConnection = 0;
        for (const line of readFileSync(trace, "utf8").split("\n").filter(Boolean)) {
            const item = JSON.parse(line), frame = item.frame;
            if (item.direction === "request" && frame.method === "initialize") legacyConnection++;
            const connection = item.connectionId ?? `legacy-${legacyConnection}`;
            const key = `${connection}:${frame.id}`;
            if (item.direction === "request") {
                if (frame.method === "tools/call") requests.set(key, item);
                continue;
            }
            if (item.direction !== "response") continue;
            const request = requests.get(key);
            if (!request) continue;
            requests.delete(key);
            const content = frame.result?.content ?? [];
            const responseText = content.filter((b: any) => b.type === "text").map((b: any) => b.text ?? "").join("\n");
            // Hints for the critic, never additional tool-error counts: caught
            // exceptions print in successful stdout, while docs also name errors.
            const diagnosticCandidates = responseText.split("\n").flatMap((line: string, index: number) =>
                /\b\w*(?:Error|Exception)\b|Traceback|truncat(?:ed|ion)|unknown venue host method/i.test(line)
                    ? [{ line: index + 1, excerpt: line.slice(0, 400) }] : []);
            const metric = { stage, connection, id: frame.id, tool: request?.frame.params?.name,
                elapsedMs: request ? (item.timeNs - request.timeNs) / 1e6 : null,
                inputCharacters: JSON.stringify(request?.frame.params?.arguments ?? {}).length,
                outcome: frame.error ? "rpc_error" : frame.result?.isError ? "tool_error" : "returned",
                error: frame.error ?? null,
                textCharacters: responseText.length,
                diagnosticCandidateCount: diagnosticCandidates.length,
                diagnosticCandidates: diagnosticCandidates.slice(0, 12),
                imageFiles: content.filter((b: any) => b.type === "image").map((b: any) => {
                    const hash = createHash("sha256").update(Buffer.from(b.data, "base64")).digest("hex");
                    const suffix = ({ "image/png": "png", "image/jpeg": "jpg", "image/webp": "webp" } as Record<string, string>)[b.mimeType] ?? "bin";
                    return `${stage}/images/${hash}.${suffix}`;
                }) };
            metrics.push(metric);
            trajectory.push({ stage, connection, id: frame.id, tool: metric.tool,
                input: request.frame.params?.arguments, outcome: metric.outcome,
                responseText, rpcError: metric.error, imageFiles: metric.imageFiles });
        }
        for (const [key, request] of requests) {
            metrics.push({ stage, connectionAndId: key, id: request.frame.id, tool: request.frame.params?.name,
                outcome: "no_response_observed", elapsedMs: null,
                inputCharacters: JSON.stringify(request.frame.params?.arguments ?? {}).length });
            trajectory.push({ stage, connectionAndId: key, id: request.frame.id,
                tool: request.frame.params?.name, input: request.frame.params?.arguments,
                outcome: "no_response_observed" });
        }
    }
    writeFileSync(join(run, "tool-metrics.json"), JSON.stringify(metrics, null, 2));
    writeFileSync(join(run, "trajectory.jsonl"), trajectory.map(event => JSON.stringify(event)).join("\n") + "\n");
}

if (phase === "prepare") {
    if (runOption) throw new Error("prepare always creates a fresh temporary directory; --run is only for later phases");
    const run = mkdtempSync(join(tmpdir(), "luma-gauntlet-"));
    const configDir = join(run, "library"); mkdirSync(configDir);
    const sourceCache = resolve(cacheOption ?? REAL_CACHE_DIR);
    const pythonEnv = join(sourceCache, "python-env");
    if (!existsSync(pythonEnv)) throw new Error(`Managed Python environment missing: ${pythonEnv}`);
    const cacheDir = join(run, "cache"); mkdirSync(cacheDir);
    symlinkSync(pythonEnv, join(cacheDir, "python-env"), "dir");
    const venueId = await createVenue(configDir, cacheDir);
    const manifest: Manifest = { version: 1, configDir, venueId, trackId: seedTrack(configDir), cacheDir };
    writeFileSync(join(run, "gauntlet.json"), JSON.stringify(manifest, null, 2));
    console.log(run);
} else {
    if (!runOption) throw new Error("--run is required");
    const run = resolve(runOption);
    const manifest: Manifest = JSON.parse(readFileSync(join(run, "gauntlet.json"), "utf8"));
    if (manifest.version !== 1 || manifest.configDir !== join(run, "library")) throw new Error("Not an isolated gauntlet directory");
    if (phase !== "inspect") {
        const evidence = join(run, phase!);
        if (existsSync(evidence)) throw new Error(`${phase} already has evidence; use a fresh prepared run for a new round`);
        mkdirSync(evidence);
        const binary = mcpBinary();
        const sourcePaths = ["backend/src", "backend/python", "backend/crates", "backend/Cargo.toml", "backend/Cargo.lock", "backend/build.rs",
            "gpui/crates/scene", "gpui/crates/render", "gpui/Cargo.toml", "gpui/Cargo.lock",
            "scripts/headless/venue-show-gauntlet.ts", "scripts/headless/gauntlet-mcp-proxy.py",
            "scripts/headless/gauntlet-mcp-smoke.py", "scripts/headless/mcp-client.ts", "harness/agent-gauntlet"];
        const diff = execFileSync("git", ["diff", "--no-ext-diff", "HEAD", "--", ...sourcePaths], { cwd: repo });
        writeFileSync(join(evidence, "source-state.diff"), diff);
        const sourceFiles = execFileSync("git", ["ls-files", "-z", "--cached", "--others", "--exclude-standard", "--", ...sourcePaths], { cwd: repo, encoding: "utf8" })
            .split("\0").filter(path => path && existsSync(join(repo, path)) && statSync(join(repo, path)).isFile()).sort();
        const hashes = Object.fromEntries(sourceFiles.map(path => [path,
            createHash("sha256").update(readFileSync(join(repo, path))).digest("hex")]));
        writeFileSync(join(evidence, "source-files.json"), JSON.stringify(hashes, null, 2));
        for (const path of sourceFiles.filter(path => path.startsWith("scripts/headless/") || path.startsWith("harness/agent-gauntlet/"))) {
            const target = join(evidence, "harness-source", path);
            mkdirSync(dirname(target), { recursive: true });
            copyFileSync(join(repo, path), target);
        }
        writeFileSync(join(evidence, "resource-state.json"), JSON.stringify({
            trackedResourcesTreeAtHead: execFileSync("git", ["rev-parse", "HEAD:resources"], { cwd: repo, encoding: "utf8" }).trim(),
            resourcesWorkingTreeStatus: execFileSync("git", ["status", "--porcelain", "--", "resources"], { cwd: repo, encoding: "utf8" }),
            submoduleCommits: execFileSync("git", ["submodule", "status"], { cwd: repo, encoding: "utf8" }),
        }, null, 2));
        writeFileSync(join(evidence, "build.json"), JSON.stringify({
            capturedAt: new Date().toISOString(), binary,
            binarySha256: createHash("sha256").update(readFileSync(binary)).digest("hex"),
            head: execFileSync("git", ["rev-parse", "HEAD"], { cwd: repo, encoding: "utf8" }).trim(),
            sourceDiffSha256: createHash("sha256").update(diff).digest("hex"),
        }, null, 2));
        const defaultPrompt = readFileSync(join(repo, "harness/agent-gauntlet", `${phase}.md`), "utf8");
        const task = customPrompt ? readFileSync(resolve(customPrompt), "utf8") : defaultPrompt;
        const prompt = `Use only the Luma MCP tools. Open venue_id=${manifest.venueId}${phase === "show" ? ` and track_id=${manifest.trackId}` : " without a track"}.\n\n${task}`;
        writeFileSync(join(evidence, "prompt.md"), prompt);
        const config = { mcpServers: { luma: { command: "python3", args: [join(repo, "scripts/headless/gauntlet-mcp-proxy.py"), evidence, binary, ...mcpArgs({ configDir: manifest.configDir, cacheDir: manifest.cacheDir, fixturePrincipal: principal })] } } };
        const mcpConfig = join(evidence, "mcp-config.json"); writeFileSync(mcpConfig, JSON.stringify(config, null, 2));
        const argv = ["--print", "--verbose", "--output-format", "stream-json", "--strict-mcp-config", "--mcp-config", mcpConfig,
            "--tools", "", "--allowedTools", "mcp__luma__*", "--disable-slash-commands", "--setting-sources", "", "--system-prompt",
            "You are a Luma venue and lighting designer. Accomplish the user's task using the supplied Luma MCP tools. Read the catalog returned by open. Inspect rendered images yourself. Report any uncertainty honestly."];
        if (model) argv.push("--model", model);
        argv.push("--", prompt);
        try {
            const smoke = [join(repo, "scripts/headless/gauntlet-mcp-smoke.py"),
                "--evidence", join(evidence, "preflight"), "--venue", manifest.venueId];
            if (phase === "show") smoke.push("--track", manifest.trackId);
            smoke.push("--", binary, ...mcpArgs({ configDir: manifest.configDir, cacheDir: manifest.cacheDir, fixturePrincipal: principal }));
            await command("python3", smoke, run, join(evidence, "preflight"), false);
            await command("claude", argv, run, join(evidence, "agent"));
        }
        finally {
            inspect(run, manifest);
            copyFileSync(join(run, "snapshot.json"), join(evidence, "snapshot.json"));
            const phaseMetrics = JSON.parse(readFileSync(join(run, "tool-metrics.json"), "utf8"))
                .filter((event: any) => event.stage === phase);
            writeFileSync(join(evidence, "tool-metrics.json"), JSON.stringify(phaseMetrics, null, 2));
            const phaseTrajectory = readFileSync(join(run, "trajectory.jsonl"), "utf8")
                .split("\n").filter(line => line && JSON.parse(line).stage === phase);
            writeFileSync(join(evidence, "trajectory.jsonl"), phaseTrajectory.join("\n") + "\n");
        }
    } else inspect(run, manifest);
    console.log(`Evidence: ${run}`);
}
