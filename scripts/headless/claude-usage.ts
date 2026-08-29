/**
 * How much of the Claude subscription's rate-limit windows is spent.
 *
 * `GET /api/oauth/usage` is what Claude Code's own `/usage` calls. Three
 * things about it are load-bearing and none are discoverable from the reply:
 *
 * - **The `User-Agent` must name Claude Code.** Without it the endpoint answers
 *   from a punitive per-token 429 bucket, so a usage *check* is what exhausts
 *   the quota. The version comes from the installed `claude` binary.
 * - **`utilization` is a percent, not a fraction.** Verified against the same
 *   payload's `limits[].percent` for the same window (`seven_day` 18.0 ↔
 *   `weekly_all` 18). Callers get fractions; the conversion lives here.
 * - **Every window is nullable.** A plan without a scoped Opus limit simply has
 *   `seven_day_opus: null`, so an absent window means "no such limit", never
 *   "zero used". `ClaudeUsage` keeps them optional to preserve that.
 *
 * The OAuth token is whatever the local Claude Code is logged in as: the
 * `CLAUDE_CODE_OAUTH_TOKEN` override, else the macOS keychain, else
 * `~/.claude/.credentials.json`. Only the access token is ever read — the
 * refresh token beside it is single-use and reusing it would revoke the
 * user's live session.
 */

import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

const USAGE_URL = "https://api.anthropic.com/api/oauth/usage";
const KEYCHAIN_SERVICE = "Claude Code-credentials";
const CREDENTIALS_FILE = join(homedir(), ".claude/.credentials.json");

import type { PlanUsage, UsageWindow } from "./usage";

// -----------------------------------------------------------------------------
// The token
// -----------------------------------------------------------------------------

/**
 * The credential blob Claude Code stores, from whichever source has one.
 *
 * The keychain lookup is scoped to the current user's account: the service name
 * alone can also match a `root` item left by a `sudo` install, and `security`
 * answers with whichever it finds first — a token that is months stale and
 * fails authentication while a perfectly good one sits beside it.
 */
function readCredentials(): { accessToken: string; expiresAt?: number } {
	const fromEnv = process.env.CLAUDE_CODE_OAUTH_TOKEN;
	if (fromEnv) return { accessToken: fromEnv };

	const parse = (raw: string) => {
		const oauth = JSON.parse(raw)?.claudeAiOauth;
		if (!oauth?.accessToken) throw new Error("no claudeAiOauth.accessToken");
		return { accessToken: oauth.accessToken as string, expiresAt: oauth.expiresAt as number };
	};

	if (process.platform === "darwin") {
		try {
			const account = process.env.USER ?? "";
			return parse(
				execFileSync(
					"security",
					["find-generic-password", "-s", KEYCHAIN_SERVICE, "-a", account, "-w"],
					{ encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] },
				),
			);
		} catch {
			// fall through to the file
		}
	}
	try {
		return parse(readFileSync(CREDENTIALS_FILE, "utf8"));
	} catch {
		throw new Error(
			"no Claude Code credentials found. Log in with `claude` (or set " +
				`CLAUDE_CODE_OAUTH_TOKEN); looked in the macOS keychain ("${KEYCHAIN_SERVICE}", ` +
				`account "${process.env.USER}") and ${CREDENTIALS_FILE}.`,
		);
	}
}

/** `claude --version` prints "2.1.247 (Claude Code)". */
function claudeVersion(): string {
	try {
		const out = execFileSync("claude", ["--version"], {
			encoding: "utf8",
			stdio: ["ignore", "pipe", "ignore"],
		});
		return out.trim().split(/\s+/)[0] || "unknown";
	} catch {
		return "unknown";
	}
}

// -----------------------------------------------------------------------------
// The fetch
// -----------------------------------------------------------------------------

function windowOf(raw: any): UsageWindow | undefined {
	if (!raw || typeof raw.utilization !== "number") return undefined;
	return {
		usedFraction: raw.utilization / 100,
		resetsAt: raw.resets_at ? new Date(raw.resets_at) : null,
	};
}

/**
 * Current subscription utilization.
 *
 * @throws if there is no usable token, or the endpoint refuses. An expired
 * access token surfaces as an authentication error naming the fix (`claude`
 * refreshes it on next launch); this module never spends the refresh token.
 */
export async function fetchClaudeUsage(): Promise<PlanUsage> {
	const { accessToken, expiresAt } = readCredentials();
	const response = await fetch(USAGE_URL, {
		headers: {
			Authorization: `Bearer ${accessToken}`,
			"anthropic-beta": "oauth-2025-04-20",
			"User-Agent": `claude-code/${claudeVersion()}`,
		},
	});
	if (!response.ok) {
		const body = (await response.text().catch(() => "")).slice(0, 300);
		const stale =
			response.status === 401 && expiresAt !== undefined && expiresAt < Date.now()
				? " The stored access token expired — run `claude` once to refresh it."
				: "";
		throw new Error(`usage check failed: HTTP ${response.status} ${body}${stale}`);
	}
	const raw = await response.json();
	// Absent windows are dropped here, so an "absent" one never reads as zero.
	const labelled: [string, UsageWindow | undefined][] = [
		["5h", windowOf(raw.five_hour)],
		["7d", windowOf(raw.seven_day)],
		["7d-opus", windowOf(raw.seven_day_opus)],
		["7d-sonnet", windowOf(raw.seven_day_sonnet)],
	];
	return {
		windows: labelled.flatMap(([label, window]) => (window ? [{ label, window }] : [])),
		weekly: windowOf(raw.seven_day),
		short: windowOf(raw.five_hour),
	};
}
