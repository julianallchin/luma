import type { AgentTool, AgentToolResult } from "@earendil-works/pi-agent-core";
import {
	type ImageContent,
	type TextContent,
	Type,
} from "@earendil-works/pi-ai";
import { z } from "zod";

export type ToolExecution = {
	toolCallId: string;
	abortSignal?: AbortSignal;
	experimentalContext?: unknown;
	/** Retained as optional call-site context for direct tool tests and callers. */
	messages?: unknown[];
};

type ModelContent =
	| { type: "text"; text: string }
	| { type: "image-data"; data: string; mediaType: string };

type ModelOutput =
	| { type: "text" | "error-text"; value: string }
	| { type: "json" | "error-json"; value: unknown }
	| { type: "execution-denied"; reason?: string }
	| { type: "content"; value: ModelContent[] };

export type LumaTool<TInput = unknown, TOutput = unknown> = {
	description: string;
	inputSchema: z.ZodType;
	execute(
		input: TInput,
		execution: ToolExecution,
	): TOutput | Promise<TOutput> | AsyncIterable<TOutput>;
	toModelOutput?(args: {
		toolCallId: string;
		input: TInput;
		output: TOutput;
	}): ModelOutput | Promise<ModelOutput>;
	label?: string;
	executionMode?: "sequential" | "parallel";
};

export type ToolSet = Record<string, LumaTool>;

export function tool<TSchema extends z.ZodType, TOutput>(definition: {
	description: string;
	inputSchema: TSchema;
	execute: (
		input: z.infer<TSchema>,
		execution: ToolExecution,
	) => TOutput | Promise<TOutput> | AsyncIterable<TOutput>;
	toModelOutput?: (args: {
		toolCallId: string;
		input: z.infer<TSchema>;
		output: TOutput;
	}) => ModelOutput | Promise<ModelOutput>;
	label?: string;
	executionMode?: "sequential" | "parallel";
}): LumaTool<z.infer<TSchema>, TOutput> {
	return definition as LumaTool<z.infer<TSchema>, TOutput>;
}

function stringify(value: unknown): string {
	if (typeof value === "string") return value;
	try {
		return JSON.stringify(value);
	} catch {
		return String(value);
	}
}

function isAsyncIterable<T>(value: unknown): value is AsyncIterable<T> {
	return (
		typeof value === "object" && value !== null && Symbol.asyncIterator in value
	);
}

async function settle<T>(value: T | Promise<T> | AsyncIterable<T>): Promise<T> {
	const resolved = await value;
	if (!isAsyncIterable<T>(resolved)) return resolved;
	let last: T | undefined;
	for await (const update of resolved) last = update;
	if (last === undefined) throw new Error("Tool stream produced no result.");
	return last;
}

function modelContent(output: ModelOutput): (TextContent | ImageContent)[] {
	switch (output.type) {
		case "text":
		case "error-text":
			return [{ type: "text", text: output.value }];
		case "json":
		case "error-json":
			return [{ type: "text", text: stringify(output.value) }];
		case "execution-denied":
			return [
				{ type: "text", text: output.reason ?? "Tool execution was denied." },
			];
		case "content":
			return output.value.map((part) =>
				part.type === "text"
					? { type: "text", text: part.text }
					: {
							type: "image",
							data: part.data,
							mimeType: part.mediaType,
						},
			);
	}
}

export async function toolResultContent(
	tool: LumaTool | undefined,
	args: { toolCallId: string; input: unknown; output: unknown },
): Promise<(TextContent | ImageContent)[]> {
	if (!tool?.toModelOutput) {
		return [{ type: "text", text: stringify(args.output) }];
	}
	return modelContent(await tool.toModelOutput(args));
}

export function toPiTools(tools: ToolSet, context?: unknown): AgentTool[] {
	return Object.entries(tools).map(([name, definition]) => ({
		name,
		label: definition.label ?? name,
		description: definition.description,
		parameters: Type.Unsafe(z.toJSONSchema(definition.inputSchema)),
		executionMode: definition.executionMode,
		execute: async (
			toolCallId,
			input,
			signal,
		): Promise<AgentToolResult<unknown>> => {
			const output = await settle(
				definition.execute(input, {
					toolCallId,
					abortSignal: signal,
					experimentalContext: context,
				}),
			);
			return {
				content: await toolResultContent(definition, {
					toolCallId,
					input,
					output,
				}),
				details: output,
			};
		},
	}));
}
