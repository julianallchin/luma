import { describe, expect, it, vi } from "vitest";
import { AuthoredSubagentMergeScheduler } from "./authored-subagent-supervisor";

describe("AuthoredSubagentMergeScheduler", () => {
	it("keeps root merges in spawn order", async () => {
		const scheduler = new AuthoredSubagentMergeScheduler();
		const first = scheduler.reserve("first");
		const second = scheduler.reserve("second");
		let firstReady = false;
		let secondReady = false;
		void first.waitForTurn().then(() => {
			firstReady = true;
		});
		void second.waitForTurn().then(() => {
			secondReady = true;
		});

		await vi.waitFor(() => expect(firstReady).toBe(true));
		expect(secondReady).toBe(false);
		first.release();
		await vi.waitFor(() => expect(secondReady).toBe(true));
		second.release();
	});

	it("runs descendants before ancestors while preserving sibling order", async () => {
		const scheduler = new AuthoredSubagentMergeScheduler();
		const root = scheduler.reserve("root");
		const firstChild = scheduler.reserve("first-child", "root");
		const grandchild = scheduler.reserve("grandchild", "first-child");
		const secondChild = scheduler.reserve("second-child", "root");
		const ready: string[] = [];
		void root.waitForTurn().then(() => ready.push("root"));
		void firstChild.waitForTurn().then(() => ready.push("first-child"));
		void grandchild.waitForTurn().then(() => ready.push("grandchild"));
		void secondChild.waitForTurn().then(() => ready.push("second-child"));

		await vi.waitFor(() => expect(ready).toEqual(["grandchild"]));
		grandchild.release();
		await vi.waitFor(() =>
			expect(ready).toEqual(["grandchild", "first-child"]),
		);
		firstChild.release();
		await vi.waitFor(() =>
			expect(ready).toEqual(["grandchild", "first-child", "second-child"]),
		);
		secondChild.release();
		await vi.waitFor(() =>
			expect(ready).toEqual([
				"grandchild",
				"first-child",
				"second-child",
				"root",
			]),
		);
		root.release();
	});

	it("rejects a nested slot after its parent is no longer active", () => {
		const scheduler = new AuthoredSubagentMergeScheduler();
		const parent = scheduler.reserve("parent");
		parent.release();
		expect(() => scheduler.reserve("late-child", "parent")).toThrow(
			/parent merge slot/i,
		);
	});
});
