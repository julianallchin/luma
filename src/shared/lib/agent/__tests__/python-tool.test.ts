import { afterEach, describe, expect, it } from "vitest";
import type { PythonCellResult, PythonToolOutput } from "@/bindings/schema";
import {
	buildPythonTool,
	pythonModelOutput,
	pythonToolLabel,
	toStoredOutput,
} from "@/shared/lib/agent/python-tool";
import { resetInvoke, setInvoke } from "@/shared/lib/tauri";

afterEach(() => resetInvoke());

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function cellResult(over: Partial<PythonCellResult> = {}): PythonCellResult {
	return {
		status: "ok",
		stdout: "",
		stderr: "",
		repr: null,
		traceback: null,
		figures: [],
		notices: [],
		durationMs: 12,
		...over,
	};
}

function output(over: Partial<PythonToolOutput> = {}): PythonToolOutput {
	return { ...toStoredOutput(cellResult()), ...over };
}

function texts(value: ReturnType<typeof pythonModelOutput>["value"]): string[] {
	return value.flatMap((b) => (b.type === "text" ? [b.text] : []));
}

function images(value: ReturnType<typeof pythonModelOutput>["value"]) {
	return value.filter((b) => b.type === "image-data");
}

type Call = { command: string; args: Record<string, unknown> };

function mockInvoke(handlers: Record<string, (args: never) => unknown>) {
	const calls: Call[] = [];
	setInvoke(async <T>(command: string, args?: Record<string, unknown>) => {
		calls.push({ command, args: args ?? {} });
		const handler = handlers[command];
		if (!handler) throw new Error(`unexpected command: ${command}`);
		return (await handler((args ?? {}) as never)) as T;
	});
	return calls;
}

// ---------------------------------------------------------------------------
// toModelOutput assembly
// ---------------------------------------------------------------------------

describe("pythonModelOutput", () => {
	it("returns a bare repr with no labels", () => {
		const out = pythonModelOutput(output({ repr: "128" }));
		expect(out.type).toBe("content");
		expect(texts(out.value)).toEqual(["128"]);
		expect(images(out.value)).toHaveLength(0);
	});

	it("labels stdout and leaves the repr bare, in order", () => {
		const out = pythonModelOutput(
			output({
				stdout: "selected threshold 0.418\n",
				repr: "{'median_lag_ms': 31.4}",
			}),
		);
		expect(texts(out.value)).toEqual([
			"stdout:\nselected threshold 0.418\n\n{'median_lag_ms': 31.4}",
		]);
	});

	it("keeps stdout emitted before a failure, then the traceback", () => {
		const out = pythonModelOutput(
			output({
				status: "error",
				stdout: "step 1 done\n",
				traceback:
					"Traceback (most recent call last):\n  ...\nValueError: bad shape\n",
			}),
		);
		const [text] = texts(out.value);
		expect(text).toBe(
			"stdout:\nstep 1 done\n\nTraceback (most recent call last):\n  ...\nValueError: bad shape",
		);
	});

	it("prefixes notices and puts them first", () => {
		const out = pythonModelOutput(
			output({
				notices: [
					"The Python kernel was restarted; earlier variables were lost.",
				],
				stdout: "ok\n",
			}),
		);
		expect(texts(out.value)[0]).toBe(
			"note: The Python kernel was restarted; earlier variables were lost.\n\nstdout:\nok",
		);
	});

	it("says so when a cell produced nothing", () => {
		expect(texts(pythonModelOutput(output()).value)).toEqual(["(no output)"]);
	});

	it("reports interruption", () => {
		const out = pythonModelOutput(
			output({ status: "interrupted", stdout: "a\n" }),
		);
		expect(texts(out.value)[0]).toContain("Cell interrupted");
	});

	it("emits one image block per figure, after the text", () => {
		const out = pythonModelOutput(
			output({
				repr: "<Figure>",
				figures: [
					{ width: 100, height: 50, base64Png: "AAA" },
					{ width: 200, height: 60, base64Png: "BBB" },
				],
			}),
		);
		expect(out.value[0]?.type).toBe("text");
		expect(images(out.value)).toEqual([
			{ type: "image-data", data: "AAA", mediaType: "image/png" },
			{ type: "image-data", data: "BBB", mediaType: "image/png" },
		]);
	});

	it("caps total figure bytes and notes what it dropped", () => {
		const big = "x".repeat(5_000_000);
		const out = pythonModelOutput(
			output({
				figures: [
					{ width: 1, height: 1, base64Png: big },
					{ width: 1, height: 1, base64Png: big },
				],
			}),
		);
		expect(images(out.value)).toHaveLength(1);
		expect(texts(out.value).at(-1)).toContain(
			"1 further figure(s) were too large",
		);
	});

	it("notes a figure whose base64 was not persisted", () => {
		const out = pythonModelOutput(
			output({ figures: [{ width: 10, height: 10 }] }),
		);
		expect(images(out.value)).toHaveLength(0);
		expect(texts(out.value).at(-1)).toContain("too large to include");
	});
});

// ---------------------------------------------------------------------------
// Stored output
// ---------------------------------------------------------------------------

describe("toStoredOutput", () => {
	it("keeps figure base64 under the persistence cap", () => {
		const stored = toStoredOutput(
			cellResult({
				figures: [
					{
						artifactRel: "outputs/a.png",
						width: 4,
						height: 2,
						base64Png: "AAA",
					},
				],
			}),
		);
		expect(stored.figures).toEqual([{ width: 4, height: 2, base64Png: "AAA" }]);
	});

	it("drops base64 for an oversized figure but keeps its geometry", () => {
		const stored = toStoredOutput(
			cellResult({
				figures: [
					{
						artifactRel: "outputs/a.png",
						width: 4,
						height: 2,
						base64Png: "x".repeat(3_000_000),
					},
				],
			}),
		);
		expect(stored.figures).toEqual([{ width: 4, height: 2 }]);
	});
});

// ---------------------------------------------------------------------------
// execute()
// ---------------------------------------------------------------------------

describe("buildPythonTool execute", () => {
	it("fills the scope out to the full wire shape", async () => {
		const calls = mockInvoke({
			run_python_cell: () => cellResult({ repr: "42" }),
		});
		const tool = buildPythonTool({
			threadId: "thread-1",
			turnMessageId: "user-1",
			getScope: () => ({ trackId: "t1", window: [0, 30] }),
		});
		const out = (await tool.execute?.(
			{ purpose: "a quick calculation to verify the result", code: "40 + 2" },
			{ toolCallId: "c1", messages: [] },
		)) as PythonToolOutput;
		expect(out.repr).toBe("42");
		expect(calls).toEqual([
			{
				command: "run_python_cell",
				args: {
					threadId: "thread-1",
					turnMessageId: "user-1",
					code: "40 + 2",
					// An agent only knows part of its scope; the rest goes over the
					// wire as the explicit nulls `PythonScopeInput` declares.
					scope: {
						trackId: "t1",
						venueId: null,
						scoreId: null,
						patternId: null,
						implementationId: null,
						window: [0, 30],
						graphDefinition: null,
					},
				},
			},
		]);
	});

	it("cancels the cell on abort and still returns the terminal result", async () => {
		const controller = new AbortController();
		const calls = mockInvoke({
			run_python_cell: async () => {
				controller.abort();
				await Promise.resolve();
				return cellResult({ status: "interrupted", stdout: "partial\n" });
			},
			cancel_python_cell: () => true,
		});
		const tool = buildPythonTool({
			threadId: "thread-2",
			turnMessageId: "user-2",
			getScope: () => null,
			abortSignal: controller.signal,
		});
		const out = (await tool.execute?.(
			{
				purpose: "a long-running cell to test interruption",
				code: "while True: pass",
			},
			{ toolCallId: "c2", messages: [] },
		)) as PythonToolOutput;
		expect(out.status).toBe("interrupted");
		expect(out.stdout).toBe("partial\n");
		expect(calls.map((c) => c.command)).toContain("cancel_python_cell");
	});

	it("does not start a cell when the turn was already stopped", async () => {
		const controller = new AbortController();
		controller.abort();
		const calls = mockInvoke({
			run_python_cell: () => cellResult(),
			cancel_python_cell: () => true,
		});
		const tool = buildPythonTool({
			threadId: "thread-stopped",
			turnMessageId: "user-stopped",
			getScope: () => null,
			abortSignal: controller.signal,
		});

		const out = (await tool.execute?.(
			{ purpose: "stopped cell", code: "edit.apply()" },
			{ toolCallId: "c-stopped", messages: [] },
		)) as PythonToolOutput;

		expect(out.status).toBe("interrupted");
		expect(calls).toEqual([]);
	});

	it("runs concurrent cells for one thread one at a time, in call order", async () => {
		let release: (() => void) | undefined;
		const gate = new Promise<void>((resolve) => {
			release = resolve;
		});
		let running = 0;
		let maxRunning = 0;
		const outputs: string[] = [];
		mockInvoke({
			run_python_cell: async (args: { code: string }) => {
				running += 1;
				maxRunning = Math.max(maxRunning, running);
				if (args.code === "first") await gate;
				running -= 1;
				outputs.push(args.code);
				return cellResult({ repr: args.code });
			},
		});
		const tool = buildPythonTool({
			threadId: "thread-serial",
			turnMessageId: "user-serial",
			getScope: () => null,
		});

		const first = tool.execute?.(
			{ purpose: "first cell", code: "first" },
			{ toolCallId: "c-a", messages: [] },
		) as Promise<PythonToolOutput>;
		const second = tool.execute?.(
			{ purpose: "second cell", code: "second" },
			{ toolCallId: "c-b", messages: [] },
		) as Promise<PythonToolOutput>;

		await Promise.resolve();
		release?.();
		const [a, b] = await Promise.all([first, second]);

		expect(maxRunning).toBe(1);
		expect(outputs).toEqual(["first", "second"]);
		expect(a.repr).toBe("first");
		expect(b.repr).toBe("second");
	});

	it("refreshes authoritative state after an invoke failure", async () => {
		mockInvoke({
			run_python_cell: () => {
				throw new Error("worker did not start");
			},
		});
		let refreshes = 0;
		const tool = buildPythonTool({
			threadId: "t",
			turnMessageId: "user-failed",
			getScope: () => null,
			afterExecute: () => {
				refreshes += 1;
			},
		});
		const out = (await tool.execute?.(
			{ purpose: "a workspace check", code: "1" },
			{ toolCallId: "c3", messages: [] },
		)) as PythonToolOutput;
		expect(out.status).toBe("failed");
		expect(out.notices[0]).toContain("worker did not start");
		expect(refreshes).toBe(1);
	});

	it("refreshes caller-owned state after a completed cell", async () => {
		mockInvoke({
			run_python_cell: () => cellResult({ repr: "<ApplyResult +1>" }),
		});
		let refreshes = 0;
		const tool = buildPythonTool({
			threadId: "thread-3",
			turnMessageId: "user-3",
			getScope: () => ({ trackId: "track-1", scoreId: "score-1" }),
			afterExecute: () => {
				refreshes += 1;
			},
		});

		await tool.execute?.(
			{ purpose: "apply score edit", code: "edit.apply()" },
			{ toolCallId: "c4", messages: [] },
		);

		expect(refreshes).toBe(1);
	});
});

// ---------------------------------------------------------------------------
// Label
// ---------------------------------------------------------------------------

describe("pythonToolLabel", () => {
	it("shows the model-authored purpose instead of code", () => {
		expect(
			pythonToolLabel({
				input: {
					purpose: "an onset analysis to find the strongest kicks",
					code: "kicks = luma.features.drum_onsets",
				},
				output: output(),
			}),
		).toEqual({
			verb: "python",
			detail: "an onset analysis to find the strongest kicks",
		});
	});

	it("does not fall back to showing code", () => {
		expect(
			pythonToolLabel({
				input: { code: "print(luma.catalog())" },
				output: output(),
			}),
		).toEqual({ verb: "python", detail: null });
	});

	it("appends a status marker when the cell did not succeed", () => {
		expect(
			pythonToolLabel({
				input: { purpose: "a validation pass", code: "boom()" },
				output: output({ status: "error", durationMs: 1500 }),
			}),
		).toEqual({
			verb: "python",
			detail: "a validation pass · error 1.5s",
		});
	});

	it("survives a missing / streaming input", () => {
		expect(pythonToolLabel({ input: undefined, output: undefined })).toEqual({
			verb: "python",
			detail: null,
		});
	});
});
