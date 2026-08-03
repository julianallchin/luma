import { type ToolSet, tool } from "ai";
import { z } from "zod";
import {
	editAuthoredWorkspaceFile,
	readAuthoredWorkspaceFile,
	writeAuthoredWorkspaceFile,
} from "./authored-workspace";

export type AuthoredWorkspaceToolScope = {
	threadId: string;
	workspaceId: string;
	fileNames: string[];
};

const MAX_TOOL_OUTPUT_BYTES = 50 * 1024;
const textEncoder = new TextEncoder();

function byteLength(value: string): number {
	return textEncoder.encode(value).byteLength;
}

function fitJsonText(
	value: string,
	buildResult: (text: string) => unknown,
): string {
	let low = 0;
	let high = value.length;
	while (low < high) {
		const middle = Math.ceil((low + high) / 2);
		if (
			byteLength(JSON.stringify(buildResult(value.slice(0, middle)))) <=
			MAX_TOOL_OUTPUT_BYTES
		) {
			low = middle;
		} else {
			high = middle - 1;
		}
	}
	return value.slice(0, low);
}

function requireFile(
	scope: AuthoredWorkspaceToolScope,
	fileName: string,
): string {
	if (!scope.fileNames.includes(fileName)) {
		throw new Error(
			`Unknown workspace file '${fileName}'. Available files: ${scope.fileNames.join(", ")}.`,
		);
	}
	return fileName;
}

async function readFile(
	scope: AuthoredWorkspaceToolScope,
	fileName: string,
): Promise<string> {
	const file = await readAuthoredWorkspaceFile({
		threadId: scope.threadId,
		workspaceId: scope.workspaceId,
		fileName: requireFile(scope, fileName),
	});
	return file.content;
}

async function writeFile(
	scope: AuthoredWorkspaceToolScope,
	fileName: string,
	content: string,
): Promise<void> {
	await writeAuthoredWorkspaceFile({
		threadId: scope.threadId,
		workspaceId: scope.workspaceId,
		fileName: requireFile(scope, fileName),
		content,
	});
}

function matchesGlob(value: string, pattern: string): boolean {
	let valueIndex = 0;
	let patternIndex = 0;
	let starIndex = -1;
	let starValueIndex = -1;
	while (valueIndex < value.length) {
		const token = pattern[patternIndex];
		if (token === "?" || token === value[valueIndex]) {
			valueIndex += 1;
			patternIndex += 1;
		} else if (token === "*") {
			starIndex = patternIndex;
			starValueIndex = valueIndex;
			patternIndex += 1;
		} else if (starIndex >= 0) {
			patternIndex = starIndex + 1;
			starValueIndex += 1;
			valueIndex = starValueIndex;
		} else {
			return false;
		}
	}
	while (pattern[patternIndex] === "*") patternIndex += 1;
	return patternIndex === pattern.length;
}

/**
 * Foam-style file tools over one bounded authored workspace. The backend owns
 * the path and document scope; the model can only name the canonical files
 * advertised for this track score or pattern graph.
 */
export function buildAuthoredWorkspaceTools(
	scope: AuthoredWorkspaceToolScope,
): ToolSet {
	const fileName = z
		.string()
		.describe(`Workspace file. Available: ${scope.fileNames.join(", ")}.`);

	return {
		ls: tool({
			description: "List the files in this authored workspace.",
			inputSchema: z.object({}),
			execute: async () => ({ files: scope.fileNames }),
		}),
		find: tool({
			description:
				"Find workspace files by a simple glob pattern such as '*.json' or 'score.*'.",
			inputSchema: z.object({ pattern: z.string().max(256) }),
			execute: async ({ pattern }) => {
				return {
					files: scope.fileNames.filter((name) => matchesGlob(name, pattern)),
				};
			},
		}),
		read: tool({
			description:
				"Read an authored workspace file. Output is capped near 50KB; narrow follow-up reads are unavailable, so use grep first when the target is known.",
			inputSchema: z.object({ file_name: fileName }),
			execute: async ({ file_name }) => {
				const content = await readFile(scope, file_name);
				const originalBytes = byteLength(content);
				const fullResult = {
					file_name,
					content,
					truncated: false,
					original_bytes: originalBytes,
				};
				if (byteLength(JSON.stringify(fullResult)) <= MAX_TOOL_OUTPUT_BYTES) {
					return fullResult;
				}
				const buildResult = (text: string) => ({
					...fullResult,
					content: text,
					truncated: true,
				});
				return buildResult(fitJsonText(content, buildResult));
			},
		}),
		grep: tool({
			description:
				"Search workspace files for literal text and return matching lines. The query is not a regular expression. Output is capped near 50KB.",
			inputSchema: z.object({
				query: z.string().min(1).max(4096),
				file_name: fileName.optional(),
			}),
			execute: async ({ query, file_name }) => {
				const names = file_name
					? [requireFile(scope, file_name)]
					: scope.fileNames;
				const matches: Array<{
					file_name: string;
					line: number;
					text: string;
				}> = [];
				let truncated = false;
				for (const name of names) {
					const content = await readFile(scope, name);
					for (const [index, text] of content.split("\n").entries()) {
						if (!text.includes(query)) continue;
						const match = { file_name: name, line: index + 1, text };
						const candidate = {
							query,
							matches: [...matches, match],
							truncated: true,
						};
						if (byteLength(JSON.stringify(candidate)) > MAX_TOOL_OUTPUT_BYTES) {
							const locator = {
								...match,
								text: "[matching line omitted because it exceeds the output limit]",
							};
							const withLocator = {
								query,
								matches: [...matches, locator],
								truncated: true,
							};
							if (
								byteLength(JSON.stringify(withLocator)) <= MAX_TOOL_OUTPUT_BYTES
							) {
								matches.push(locator);
							}
							truncated = true;
							return { query, matches, truncated };
						}
						matches.push(match);
						if (matches.length >= 200) {
							truncated = true;
							return { query, matches, truncated };
						}
					}
				}
				return { query, matches, truncated };
			},
		}),
		write: tool({
			description:
				"Replace a complete authored workspace file. Read it first and preserve every part not intentionally changed.",
			inputSchema: z.object({ file_name: fileName, content: z.string() }),
			execute: async ({ file_name, content }) => {
				await writeFile(scope, file_name, content);
				return { ok: true, file_name };
			},
		}),
		edit: tool({
			description:
				"Replace an exact text fragment in an authored workspace file. By default the old text must occur exactly once.",
			inputSchema: z.object({
				file_name: fileName,
				old_text: z.string().min(1),
				new_text: z.string(),
				replace_all: z.boolean().optional(),
			}),
			execute: async ({ file_name, old_text, new_text, replace_all }) => {
				const result = await editAuthoredWorkspaceFile({
					threadId: scope.threadId,
					workspaceId: scope.workspaceId,
					fileName: requireFile(scope, file_name),
					oldText: old_text,
					newText: new_text,
					replaceAll: replace_all ?? false,
				});
				return {
					ok: true,
					file_name: result.fileName,
					replacements: result.replacements,
				};
			},
		}),
	};
}
