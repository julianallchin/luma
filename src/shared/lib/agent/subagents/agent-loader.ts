import { parseFrontmatter } from "@/shared/lib/agent/frontmatter";
import { BUNDLED_AGENT_DEFINITIONS } from "./bundled-agents";
import type {
	AgentConfig,
	AgentDefinition,
	SubagentThinkingLevel,
} from "./types";

const GENERIC_BASE = `# Role
You are a general-purpose agent working inside Luma.
Use the capabilities available to you to complete the assigned task.`;

const THINKING_LEVELS = new Set<SubagentThinkingLevel>([
	"off",
	"minimal",
	"low",
	"medium",
	"high",
	"xhigh",
]);

function parseThinkingLevel(value?: string): SubagentThinkingLevel | undefined {
	if (!value) return undefined;
	if (!THINKING_LEVELS.has(value as SubagentThinkingLevel)) {
		throw new Error(`Invalid subagent thinking level: "${value}".`);
	}
	return value as SubagentThinkingLevel;
}

function fromRaw(content: string): AgentConfig {
	const { data, body } = parseFrontmatter(content);
	const name = data.name?.trim();
	if (!name) throw new Error("Agent definition is missing a name.");
	return {
		name,
		description: data.description ?? "",
		systemPrompt: body,
		promptMode: data.prompt_mode === "append" ? "append" : "replace",
		toolNames: data.tools
			? data.tools
					.split(",")
					.map((toolName) => toolName.trim())
					.filter(Boolean)
			: [],
		model: data.model || undefined,
		thinkingLevel: parseThinkingLevel(data.thinking),
	};
}

function normalizeDefinition(definition: AgentDefinition): AgentConfig {
	const config =
		typeof definition === "string" ? fromRaw(definition) : definition;
	if (!config.name.trim())
		throw new Error("Agent definition is missing a name.");
	return {
		...config,
		name: config.name.trim(),
		description: config.description.trim(),
		systemPrompt: config.systemPrompt.trim(),
		toolNames: [...new Set(config.toolNames.map((name) => name.trim()))].filter(
			Boolean,
		),
	};
}

export class AgentLoader {
	private readonly agents = new Map<string, AgentConfig>();

	constructor(
		options: {
			definitions?: AgentDefinition[];
			includeBundled?: boolean;
		} = {},
	) {
		if (options.includeBundled !== false) {
			for (const definition of BUNDLED_AGENT_DEFINITIONS) {
				this.load(definition);
			}
		}
		for (const definition of options.definitions ?? []) this.load(definition);
	}

	/** Add or replace a definition. Later definitions win by name. */
	load(definition: AgentDefinition): AgentConfig {
		const config = normalizeDefinition(definition);
		this.agents.set(config.name, config);
		return config;
	}

	get(name: string): AgentConfig | undefined {
		return this.agents.get(name);
	}

	list(): AgentConfig[] {
		return [...this.agents.values()];
	}

	buildSystemPrompt(
		config: AgentConfig,
		options: { parentSystemPrompt?: string; environment?: string } = {},
	): string {
		const activeAgentTag = `<active_agent name="${config.name}"/>`;
		const environment = options.environment?.trim()
			? `# Environment\n${options.environment.trim()}`
			: "# Environment\nApplication: Luma desktop";

		if (config.promptMode === "append") {
			const identity = options.parentSystemPrompt ?? GENERIC_BASE;
			const bridge = `<sub_agent_context>
You are operating as a subagent invoked for one specific task.
- The task prompt is self-contained; you have no parent conversation history.
- Work only inside the child workspace exposed by your tools.
- Make independent tool calls in parallel when useful.
- Be concise but complete in your final response.
</sub_agent_context>`;
			const instructions = config.systemPrompt
				? `\n\n<agent_instructions>\n${config.systemPrompt}\n</agent_instructions>`
				: "";
			return `${identity}\n\n${bridge}\n\n${activeAgentTag}\n\n${environment}${instructions}`;
		}

		const header = `You are a Luma subagent invoked to handle one specific task autonomously.\n\n${environment}`;
		return `${activeAgentTag}\n\n${header}\n\n${config.systemPrompt}`;
	}
}
