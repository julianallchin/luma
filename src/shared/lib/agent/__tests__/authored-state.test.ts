import { afterEach, describe, expect, it } from "vitest";
import { listAuthoredHistory } from "@/shared/lib/agent/authored-state";
import { resetInvoke, setInvoke } from "@/shared/lib/tauri";

afterEach(() => resetInvoke());

describe("authored history", () => {
	it("forwards the opaque first-parent cursor when loading older revisions", async () => {
		const calls: Array<{
			command: string;
			args: Record<string, unknown> | undefined;
		}> = [];
		setInvoke(async <T>(command: string, args?: Record<string, unknown>) => {
			calls.push({ command, args });
			return { entries: [], nextCursor: null } as T;
		});

		await listAuthoredHistory("thread-1", "commit-cursor", 25);

		expect(calls).toEqual([
			{
				command: "authored_state_list_history",
				args: {
					threadId: "thread-1",
					cursor: "commit-cursor",
					limit: 25,
				},
			},
		]);
	});
});
