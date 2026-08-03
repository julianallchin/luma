import { afterEach, describe, expect, it } from "vitest";
import { resetInvoke, setInvoke } from "@/shared/lib/tauri";
import { exportScoreDsl, importScoreDsl, validateScoreDsl } from "./index";

const scope = {
	scoreId: "score-a",
	trackId: "track-a",
	venueId: "venue-a",
};

afterEach(() => resetInvoke());

describe("score DSL backend client", () => {
	it("routes export, validation, and import through the Rust command seam", async () => {
		const calls: Array<[string, Record<string, unknown> | undefined]> = [];
		setInvoke(async <T>(command: string, args?: Record<string, unknown>) => {
			calls.push([command, args]);
			if (command === "score_dsl_export") {
				return {
					source: "layer 0:",
					revision: "rev",
					clipCount: 0,
				} as T;
			}
			if (command === "score_dsl_validate") {
				return {
					valid: true,
					baseRevision: "rev",
					clipCount: 0,
					diagnostics: [],
				} as T;
			}
			if (command === "score_dsl_import") {
				return {
					documentId: "document-a",
					revisionId: "revision-a",
					changed: true,
					document: { kind: "track_score", revision: "next-rev" },
				} as T;
			}
			return undefined as T;
		});

		await exportScoreDsl(scope, false);
		await validateScoreDsl(scope, "layer 0:");
		const imported = await importScoreDsl(scope, "layer 0:", "rev");

		expect(calls).toEqual([
			["score_dsl_export", { ...scope, includeClipIds: false }],
			["score_dsl_validate", { ...scope, source: "layer 0:" }],
			[
				"score_dsl_import",
				{
					...scope,
					source: "layer 0:",
					baseRevision: "rev",
					operationId: expect.any(String),
				},
			],
		]);
		expect(imported).toEqual({
			documentId: "document-a",
			revisionId: "revision-a",
			changed: true,
			document: { kind: "track_score", revision: "next-rev" },
		});
	});

	it("retries a lost import response with the same operation identity", async () => {
		const calls: Record<string, unknown>[] = [];
		setInvoke(async <T>(command: string, args?: Record<string, unknown>) => {
			expect(command).toBe("score_dsl_import");
			calls.push(args ?? {});
			if (calls.length === 1) throw new Error("response lost");
			return {
				documentId: "document-a",
				revisionId: "revision-a",
				changed: true,
				document: { kind: "track_score", revision: "next-rev" },
			} as T;
		});

		await importScoreDsl(scope, "layer 0:", "rev");
		expect(calls).toHaveLength(2);
		expect(calls[1]).toEqual(calls[0]);
	});
});
