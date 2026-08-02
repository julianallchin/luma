import { describe, expect, it } from "vitest";
import { createAgentChat } from "./create-agent-chat";

function chat() {
	return createAgentChat<{ name: string }>({
		agentKind: "track_copilot",
		subjectKind: "track",
		createModel: () => null,
		buildTools: () => ({}),
		buildSystem: () => "",
		vocab: {
			verbs: {},
			formatLabel: () => ({ verb: "", detail: null }),
		},
	});
}

describe("createAgentChat bridge scopes", () => {
	it("keeps bridges for the same subject isolated by venue and score", () => {
		const agent = chat();
		const a = { name: "venue A / score A" };
		const b = { name: "venue B / score B" };

		agent.registerBridge("track-1", a, {
			principalId: "user-a",
			venueId: "venue-a",
			scoreId: "score-a",
		});
		agent.registerBridge("track-1", b, {
			principalId: "user-a",
			venueId: "venue-b",
			scoreId: "score-b",
		});

		expect(
			agent.getBridge("track-1", {
				principalId: "user-a",
				venueId: "venue-a",
				scoreId: "score-a",
			}),
		).toBe(a);
		expect(
			agent.getBridge("track-1", {
				principalId: "user-a",
				venueId: "venue-b",
				scoreId: "score-b",
			}),
		).toBe(b);
		expect(agent.getBridge("track-1")).toBeNull();
	});

	it("does not let stale cleanup remove a newer bridge", () => {
		const agent = chat();
		const init = {
			principalId: "user-a",
			venueId: "venue-a",
			scoreId: "score-a",
		};
		const first = { name: "first" };
		const second = { name: "second" };
		const unregisterFirst = agent.registerBridge("track-1", first, init);
		const unregisterSecond = agent.registerBridge("track-1", second, init);

		unregisterFirst();
		expect(agent.getBridge("track-1", init)).toBe(second);

		unregisterSecond();
		expect(agent.getBridge("track-1", init)).toBeNull();
	});

	it("restores an older live registration when a newer one unmounts", () => {
		const agent = chat();
		const init = {
			principalId: "user-a",
			venueId: "venue-a",
			scoreId: "score-a",
		};
		const first = { name: "first" };
		const second = { name: "second" };
		const unregisterFirst = agent.registerBridge("track-1", first, init);
		const unregisterSecond = agent.registerBridge("track-1", second, init);

		unregisterSecond();
		expect(agent.getBridge("track-1", init)).toBe(first);

		unregisterFirst();
		expect(agent.getBridge("track-1", init)).toBeNull();
	});

	it("never reuses an in-memory scope across account principals", () => {
		const agent = chat();
		const alice = { name: "alice" };
		const bob = { name: "bob" };
		const shared = { venueId: "venue-a", scoreId: "score-a" };

		agent.registerBridge("track-1", alice, {
			...shared,
			principalId: "alice",
		});
		agent.registerBridge("track-1", bob, {
			...shared,
			principalId: "bob",
		});

		expect(
			agent.getBridge("track-1", { ...shared, principalId: "alice" }),
		).toBe(alice);
		expect(agent.getBridge("track-1", { ...shared, principalId: "bob" })).toBe(
			bob,
		);
	});
});
