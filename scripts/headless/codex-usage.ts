/**
 * How much of the ChatGPT plan's rate-limit windows Codex has spent.
 *
 * `GET {chatgpt_base_url}/wham/usage` is what Codex's own `/usage` calls,
 * bearing the access token `codex login` stored in `$CODEX_HOME/auth.json`.
 * The reply's `rate_limit` has a `primary_window` (5 hours on Plus) and a
 * `secondary_window` (7 days); either may be null, and `used_percent` is an
 * integer percent. The `ChatGPT-Account-Id` header selects the workspace the
 * token was minted for — without it a multi-workspace account answers for the
 * wrong one.
 *
 * Only the access token is read. The refresh token beside it belongs to the
 * CLI; Codex refreshes on its next launch, and an expired token here is a
 * clear error, not a silent refresh.
 */

import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import type { PlanUsage, UsageWindow } from "./usage";

const AUTH_FILE = join(process.env.CODEX_HOME ?? join(homedir(), ".codex"), "auth.json");
const BASE_URL = process.env.CODEX_CHATGPT_BASE_URL ?? "https://chatgpt.com/backend-api";

function readToken(): { accessToken: string; accountId?: string } {
	let raw: string;
	try {
		raw = readFileSync(AUTH_FILE, "utf8");
	} catch {
		throw new Error(`no Codex credentials at ${AUTH_FILE}. Run \`codex login\` first.`);
	}
	const tokens = JSON.parse(raw)?.tokens;
	if (!tokens?.access_token) {
		throw new Error(`${AUTH_FILE} has no ChatGPT access token — is this an API-key login?`);
	}
	return { accessToken: tokens.access_token, accountId: tokens.account_id };
}

function windowOf(raw: any): UsageWindow | undefined {
	if (!raw || typeof raw.used_percent !== "number") return undefined;
	return {
		usedFraction: raw.used_percent / 100,
		resetsAt: raw.reset_at ? new Date(raw.reset_at * 1000) : null,
	};
}

/**
 * Current plan utilization.
 *
 * @throws if there is no usable token, or the endpoint refuses.
 */
export async function fetchCodexUsage(): Promise<PlanUsage> {
	const { accessToken, accountId } = readToken();
	const response = await fetch(`${BASE_URL}/wham/usage`, {
		headers: {
			Authorization: `Bearer ${accessToken}`,
			...(accountId ? { "ChatGPT-Account-Id": accountId } : {}),
		},
	});
	if (!response.ok) {
		const body = (await response.text().catch(() => "")).slice(0, 300);
		const stale =
			response.status === 401 ? " The stored token may have expired — run `codex` once." : "";
		throw new Error(`usage check failed: HTTP ${response.status} ${body}${stale}`);
	}
	const raw = await response.json();
	const limit = raw.rate_limit ?? {};
	const short = windowOf(limit.primary_window);
	const weekly = windowOf(limit.secondary_window);
	const hours = (w: any) => `${Math.round((w?.limit_window_seconds ?? 0) / 3600)}h`;
	const labelled: [string, UsageWindow | undefined][] = [
		[hours(limit.primary_window), short],
		[`${Math.round((limit.secondary_window?.limit_window_seconds ?? 0) / 86400)}d`, weekly],
	];
	return {
		windows: labelled.flatMap(([label, window]) => (window ? [{ label, window }] : [])),
		weekly,
		short,
	};
}
