import { tool } from "ai";
import { z } from "zod";
import type { SubagentManager } from "./subagent-manager";
import type { AgentToolOutput, SteerSubagentToolOutput } from "./types";

export type CreateSubagentToolsOptions = {
	/** Read lazily so append-mode children inherit this exact parent turn. */
	getParentSystemPrompt: () => string | undefined;
	availableModels?: string[] | (() => string[]);
	/** Owning child run when these tools are installed recursively. */
	parentSubagentId?: string;
};

function modelsOf(options: CreateSubagentToolsOptions): string[] {
	return typeof options.availableModels === "function"
		? options.availableModels()
		: (options.availableModels ?? []);
}

function agentToolDescription<Context>(
	manager: SubagentManager<Context>,
	options: CreateSubagentToolsOptions,
): string {
	const agents = manager.agentLoader
		.list()
		.map((agent) => {
			const model = agent.model ? ` (default model: ${agent.model})` : "";
			return `- ${agent.name}: ${agent.description}${model}`;
		})
		.join("\n");
	const models = modelsOf(options);
	const modelSection = models.length
		? `\n\nAvailable model overrides:\n${models.map((model) => `- ${model}`).join("\n")}`
		: "";
	return `Launch a new agent to handle a complex, multi-step task autonomously.

Available agent types:
${agents || "- none"}

Give the child a specific, self-contained prompt: it has no parent conversation history. State clearly whether it should inspect or change the current document.

- Always provide a short 3-5 word description for the UI.
- Foreground is the default and returns the result directly.
- For independent parallel work, make multiple Agent calls in one response with run_in_background true, then collect each with get_subagent_result.
- Use steer_subagent to send mid-run guidance to a background child.
- Verify a child's claimed edits against the resulting authored state.${modelSection}`;
}

export function createSubagentTools<Context>(
	manager: SubagentManager<Context>,
	options: CreateSubagentToolsOptions,
) {
	const Agent = tool({
		get description() {
			return agentToolDescription(manager, options);
		},
		inputSchema: z.object({
			subagent_type: z
				.string()
				.describe("Agent type to spawn from the available types above."),
			prompt: z
				.string()
				.min(1)
				.describe(
					"Specific, self-contained task; the child has no memory of this conversation.",
				),
			description: z
				.string()
				.min(1)
				.describe("Short 3-5 word description shown in the UI."),
			run_in_background: z
				.boolean()
				.optional()
				.describe(
					"Return an agent id immediately after setup instead of waiting for completion.",
				),
			model: z
				.string()
				.optional()
				.describe("Optional model id from the available models above."),
			inherit_context: z
				.boolean()
				.optional()
				.describe(
					"False gives append-mode agents a generic identity instead of the parent system prompt. Replace-mode agents ignore this option.",
				),
		}),
		execute: async (input, execution): Promise<AgentToolOutput> => {
			const models = modelsOf(options);
			if (input.model && models.length > 0 && !models.includes(input.model)) {
				throw new Error(
					`Unknown model "${input.model}". Available: ${models.join(", ")}`,
				);
			}
			const record = await manager.spawn(input.subagent_type, input.prompt, {
				parentSystemPrompt:
					input.inherit_context === false
						? undefined
						: options.getParentSystemPrompt(),
				model: input.model,
				parentToolCallId: execution.toolCallId,
				parentSubagentId: options.parentSubagentId,
				setupSignal: execution.abortSignal,
			});
			if (input.run_in_background) {
				const subagent = manager.get(record.id);
				if (!subagent) throw new Error(`Subagent "${record.id}" disappeared.`);
				return { agent_id: record.id, status: "running", subagent };
			}

			try {
				const result = await manager.getResult(
					record.id,
					execution.abortSignal,
					"abort-before-finalization",
				);
				const subagent = manager.get(record.id);
				if (!subagent) throw new Error(`Subagent "${record.id}" disappeared.`);
				return {
					agent_id: record.id,
					status: "completed",
					result,
					subagent,
				};
			} catch (error) {
				if (execution.abortSignal?.aborted) manager.abortOne(record.id);
				throw error;
			}
		},
		toModelOutput: ({ output }) => ({
			type: "text" as const,
			value:
				output.status === "completed"
					? (output.result ?? "")
					: JSON.stringify({ agent_id: output.agent_id }),
		}),
	});

	const get_subagent_result = tool({
		description:
			"Await and retrieve a background subagent result. Stopping this parent turn cancels only the wait; the child keeps running.",
		inputSchema: z.object({
			agent_id: z.string().describe("Id returned by a background Agent call."),
		}),
		execute: async ({ agent_id }, execution): Promise<AgentToolOutput> => {
			const result = await manager.getResult(agent_id, execution.abortSignal);
			const subagent = manager.get(agent_id);
			if (!subagent) throw new Error(`Subagent "${agent_id}" disappeared.`);
			return { agent_id, status: "completed", result, subagent };
		},
		toModelOutput: ({ output }) => ({
			type: "text" as const,
			value: output.result ?? "",
		}),
	});

	const steer_subagent = tool({
		description:
			"Send guidance to a running background subagent. It is delivered at the next model step.",
		inputSchema: z.object({
			agent_id: z.string().describe("Id of the running background agent."),
			message: z
				.string()
				.min(1)
				.describe("Correction or additional context for the child."),
		}),
		execute: async ({
			agent_id,
			message,
		}): Promise<SteerSubagentToolOutput> => {
			manager.steer(agent_id, message);
			return { agent_id, status: "running" };
		},
		toModelOutput: ({ output }) => ({
			type: "text" as const,
			value: `Steered agent ${output.agent_id}.`,
		}),
	});

	return { Agent, get_subagent_result, steer_subagent };
}
