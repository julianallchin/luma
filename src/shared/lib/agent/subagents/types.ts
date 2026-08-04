import type {
	JSONValue,
	LanguageModel,
	StopCondition,
	ToolSet,
	UIMessage,
	UIMessageChunk,
} from "ai";

export type SubagentPromptMode = "replace" | "append";

/** Pi-compatible reasoning levels. Model adapters decide how to map them. */
export type SubagentThinkingLevel =
	| "off"
	| "minimal"
	| "low"
	| "medium"
	| "high"
	| "xhigh";

/** A data-defined agent type. Empty toolNames means use the full child-scoped
 * tool pool supplied by the parent integration. */
export type AgentConfig = {
	name: string;
	description: string;
	systemPrompt: string;
	promptMode: SubagentPromptMode;
	toolNames: string[];
	model?: string;
	thinkingLevel?: SubagentThinkingLevel;
};

export type AgentDefinition = AgentConfig | string;

export type SubagentStatus = "running" | "completed" | "error" | "aborted";

export type SubagentSnapshot = {
	id: string;
	type: string;
	status: SubagentStatus;
	startedAt: number;
	lastActivityAt: number;
	finishedAt?: number;
	messages: UIMessage[];
	result?: string;
	error?: string;
	parentToolCallId?: string;
};

export type AgentRecord = SubagentSnapshot & {
	promise: Promise<string>;
};

export type SubagentEventMeta = {
	subagentId: string;
	subagentType: string;
	parentToolCallId?: string;
	timestamp: number;
};

export type SubagentEvent = SubagentEventMeta &
	(
		| { event: { type: "start" } }
		| { event: { type: "ui-message-chunk"; chunk: UIMessageChunk } }
		| {
				event: {
					type: "end";
					status: Exclude<SubagentStatus, "running">;
					result?: string;
					error?: string;
				};
		  }
	);

export type SubagentEventCallback = (event: SubagentEvent) => void;

export type SubagentRunRequest = {
	systemPrompt: string;
	prompt: string;
	tools: ToolSet;
	model?: string;
	thinkingLevel?: SubagentThinkingLevel;
	abortSignal: AbortSignal;
	drainSteering: () => string[];
	onUIMessageChunk: (chunk: UIMessageChunk) => void | Promise<void>;
	context?: unknown;
};

export type SubagentRunner = (request: SubagentRunRequest) => Promise<string>;

export type CreateAiSdkSubagentRunnerOptions = {
	createModel: (modelId?: string) => LanguageModel;
	stopWhen?: StopCondition<ToolSet>;
	providerOptions?: (args: {
		modelId?: string;
		thinkingLevel?: SubagentThinkingLevel;
	}) => Record<string, Record<string, JSONValue | undefined>> | undefined;
};

export type SpawnSubagentOptions = {
	parentSystemPrompt?: string;
	model?: string;
	parentToolCallId?: string;
	/** Durable user message that originated this delegation tree. Recursive
	 * children keep the same id so domain tools share the root turn's authority. */
	turnMessageId?: string;
	/** Owning child run for recursive delegation. Root spawns omit this. */
	parentSubagentId?: string;
	setupSignal?: AbortSignal;
};

export type SubagentRunOutcome =
	| { status: "completed"; result: string }
	| { status: "error"; error: string }
	| { status: "aborted"; error?: string };

export type PreparedSubagentRun<Context = unknown> = {
	/** Parent-equivalent domain tools rebound to this child's authored state. */
	tools: ToolSet;
	/** Opaque integration state passed to the runner and lifecycle hooks. */
	context?: Context;
	/** Commit/merge the child's authored result before its record resolves. */
	finalize?: (args: {
		id: string;
		agent: AgentConfig;
		outcome: SubagentRunOutcome;
		context: Context | undefined;
	}) => string | undefined | Promise<string | undefined>;
	/** Release child-scoped resources after finalization or failure. */
	cleanup?: (args: {
		id: string;
		agent: AgentConfig;
		outcome: SubagentRunOutcome;
		context: Context | undefined;
	}) => void | Promise<void>;
};

export type PrepareSubagentSpawn<Context = unknown> = (args: {
	id: string;
	agent: AgentConfig;
	prompt: string;
	parentToolCallId?: string;
	parentSubagentId?: string;
	/** Durable user message that originated this delegation tree. */
	turnMessageId?: string;
	abortSignal: AbortSignal;
}) => PreparedSubagentRun<Context> | Promise<PreparedSubagentRun<Context>>;

export type AgentToolOutput = {
	agent_id: string;
	status: "running" | "completed";
	result?: string;
	/** Durable child state for transcript replay; omitted from model output. */
	subagent: SubagentSnapshot;
};

export type SteerSubagentToolOutput = {
	agent_id: string;
	status: "running";
};
