import type { ToolSet } from "@/shared/lib/agent/agent-tool";
import { userChatMessage } from "@/shared/lib/agent/messages";
import { AgentLoader } from "./agent-loader";
import { createSubagentTools } from "./tools";
import type {
	AgentConfig,
	AgentRecord,
	PreparedSubagentRun,
	PrepareSubagentSpawn,
	SpawnSubagentOptions,
	SubagentEvent,
	SubagentEventCallback,
	SubagentRunner,
	SubagentRunOutcome,
	SubagentSnapshot,
} from "./types";

type InternalRecord<Context> = AgentRecord & {
	abortController: AbortController;
	phase: "running" | "finalizing" | "terminal";
	steering: string[];
	steeringListeners: Set<(message: string) => void>;
	prepared: PreparedSubagentRun<Context>;
};

export type SubagentManagerOptions<Context = unknown> = {
	runner: SubagentRunner;
	prepareSpawn: PrepareSubagentSpawn<Context>;
	agentLoader?: AgentLoader;
	environment?: string;
	onEvent?: SubagentEventCallback;
	createId?: () => string;
	availableModels?: string[] | (() => string[]);
};

export type SubagentSnapshotListener = (snapshots: SubagentSnapshot[]) => void;

export type SubagentResultAbortMode = "detach" | "abort-before-finalization";

function messageOf(error: unknown): string {
	return error instanceof Error ? error.message : String(error);
}

function aborted(error: unknown, signal: AbortSignal): boolean {
	return (
		signal.aborted ||
		(error instanceof DOMException && error.name === "AbortError") ||
		(error instanceof Error && error.name === "AbortError")
	);
}

function outcomeError(outcome: SubagentRunOutcome): Error {
	if (outcome.status === "completed") {
		return new Error("Completed subagent cannot be converted to an error.");
	}
	const error = new Error(
		outcome.status === "aborted"
			? (outcome.error ?? "Subagent was aborted.")
			: outcome.error,
	);
	if (outcome.status === "aborted") error.name = "AbortError";
	return error;
}

/** Reject an await when its owning parent turn stops without touching the child. */
export function raceAbort<T>(
	promise: Promise<T>,
	signal?: AbortSignal,
): Promise<T> {
	if (!signal) return promise;
	if (signal.aborted) {
		const error = new Error("Aborted");
		error.name = "AbortError";
		return Promise.reject(error);
	}
	return new Promise<T>((resolve, reject) => {
		const onAbort = () => {
			cleanup();
			const error = new Error("Aborted");
			error.name = "AbortError";
			reject(error);
		};
		const cleanup = () => signal.removeEventListener("abort", onAbort);
		signal.addEventListener("abort", onAbort, { once: true });
		promise.then(
			(value) => {
				cleanup();
				resolve(value);
			},
			(error: unknown) => {
				cleanup();
				reject(error);
			},
		);
	});
}

export class SubagentManager<Context = unknown> {
	readonly agentLoader: AgentLoader;
	private readonly records = new Map<string, InternalRecord<Context>>();
	private readonly listeners = new Set<SubagentSnapshotListener>();
	private readonly setupControllers = new Set<AbortController>();
	private readonly pendingSetups = new Set<Promise<void>>();
	private readonly runner: SubagentRunner;
	private readonly prepareSpawn: PrepareSubagentSpawn<Context>;
	private readonly environment?: string;
	private readonly onEvent?: SubagentEventCallback;
	private readonly createId: () => string;
	private readonly availableModels?: string[] | (() => string[]);

	constructor(options: SubagentManagerOptions<Context>) {
		this.runner = options.runner;
		this.prepareSpawn = options.prepareSpawn;
		this.agentLoader = options.agentLoader ?? new AgentLoader();
		this.environment = options.environment;
		this.onEvent = options.onEvent;
		this.createId = options.createId ?? (() => crypto.randomUUID());
		this.availableModels = options.availableModels;
	}

	/**
	 * Allocate an isolated child workspace, then start the child immediately.
	 * Background callers still await allocation so a returned id is always live.
	 */
	async spawn(
		agentType: string,
		prompt: string,
		options: SpawnSubagentOptions = {},
	): Promise<AgentRecord> {
		const config = this.agentLoader.get(agentType);
		if (!config) {
			const available = this.agentLoader
				.list()
				.map((agent) => agent.name)
				.join(", ");
			throw new Error(
				`Unknown agent type: "${agentType}". Available: ${available || "none"}`,
			);
		}

		const id = this.createId();
		const abortController = new AbortController();
		this.setupControllers.add(abortController);
		let settleSetup!: () => void;
		const setupSettled = new Promise<void>((resolve) => {
			settleSetup = resolve;
		});
		this.pendingSetups.add(setupSettled);
		const stopSetup = () => abortController.abort();
		if (options.setupSignal?.aborted) stopSetup();
		else
			options.setupSignal?.addEventListener("abort", stopSetup, { once: true });

		let prepared: PreparedSubagentRun<Context>;
		try {
			prepared = await this.prepareSpawn({
				id,
				agent: config,
				prompt,
				parentToolCallId: options.parentToolCallId,
				parentSubagentId: options.parentSubagentId,
				turnMessageId: options.turnMessageId,
				abortSignal: abortController.signal,
			});
			if (abortController.signal.aborted) {
				await prepared.cleanup?.({
					id,
					agent: config,
					outcome: { status: "aborted" },
					context: prepared.context,
				});
				throw outcomeError({ status: "aborted" });
			}
		} finally {
			options.setupSignal?.removeEventListener("abort", stopSetup);
			this.setupControllers.delete(abortController);
			this.pendingSetups.delete(setupSettled);
			settleSetup();
		}

		const systemPrompt = this.agentLoader.buildSystemPrompt(config, {
			parentSystemPrompt: options.parentSystemPrompt,
			environment: this.environment,
		});
		const recursiveTools = createSubagentTools(this, {
			getParentSystemPrompt: () => systemPrompt,
			availableModels: this.availableModels,
			parentSubagentId: id,
			turnMessageId: options.turnMessageId,
		});
		const tools = this.filterTools(
			{ ...prepared.tools, ...recursiveTools },
			config,
		);
		const record: InternalRecord<Context> = {
			id,
			type: agentType,
			status: "running",
			startedAt: Date.now(),
			lastActivityAt: Date.now(),
			messages: [userChatMessage(prompt, `${id}:prompt`)],
			parentToolCallId: options.parentToolCallId,
			promise: Promise.resolve(""),
			abortController,
			phase: "running",
			steering: [],
			steeringListeners: new Set(),
			prepared,
		};
		this.records.set(id, record);
		this.emit(record, { event: { type: "start" } });
		this.notify();

		record.promise = this.run(record, config, {
			systemPrompt,
			prompt,
			tools,
			model: options.model ?? config.model,
		});
		// A background child may never be joined. Mark rejection handled while
		// preserving it for a later getResult call.
		record.promise.catch(() => undefined);
		return record;
	}

	get(id: string): SubagentSnapshot | undefined {
		const record = this.records.get(id);
		return record ? this.snapshot(record) : undefined;
	}

	list(): SubagentSnapshot[] {
		return [...this.records.values()].map((record) => this.snapshot(record));
	}

	subscribe(listener: SubagentSnapshotListener): () => void {
		this.listeners.add(listener);
		listener(this.list());
		return () => this.listeners.delete(listener);
	}

	getResult(
		id: string,
		signal?: AbortSignal,
		abortMode: SubagentResultAbortMode = "detach",
	): Promise<string> {
		const record = this.records.get(id);
		if (!record) throw new Error(`No subagent with id: "${id}".`);
		if (abortMode === "detach") return raceAbort(record.promise, signal);
		return this.raceForegroundAbort(record, signal);
	}

	steer(id: string, message: string): void {
		const record = this.records.get(id);
		if (!record) throw new Error(`No subagent with id: "${id}".`);
		if (record.status !== "running" || record.phase !== "running") {
			throw new Error(`Subagent "${id}" is not running.`);
		}
		if (record.steeringListeners.size === 0) {
			record.steering.push(message);
		} else {
			for (const listener of record.steeringListeners) listener(message);
		}
		record.lastActivityAt = Date.now();
		this.notify();
	}

	abortOne(id: string): void {
		const record = this.records.get(id);
		if (!record || record.status !== "running" || record.phase !== "running") {
			return;
		}
		record.status = "aborted";
		record.lastActivityAt = Date.now();
		record.abortController.abort();
		this.notify();
	}

	/** Session teardown only. Parent-turn Stop should target foreground children. */
	abort(): void {
		for (const id of this.records.keys()) this.abortOne(id);
	}

	/** Abort all setup/runs and wait until finalization and cleanup have settled. */
	async dispose(): Promise<void> {
		for (const controller of this.setupControllers) controller.abort();
		this.abort();
		await Promise.allSettled([...this.pendingSetups]);
		// A setup may have completed just before its abort landed and registered a
		// record after the first sweep.
		this.abort();
		await Promise.allSettled(
			[...this.records.values()].map((record) => record.promise),
		);
	}

	private filterTools(tools: ToolSet, config: AgentConfig): ToolSet {
		if (config.toolNames.length === 0) return tools;
		const allowed = new Set(config.toolNames);
		return Object.fromEntries(
			Object.entries(tools).filter(([name]) => allowed.has(name)),
		);
	}

	/**
	 * Foreground cancellation owns the child only while setup/model execution is
	 * reversible. Once finalization starts, keep awaiting the same promise so a
	 * committed workspace can never be reported as aborted or left detached.
	 */
	private raceForegroundAbort(
		record: InternalRecord<Context>,
		signal?: AbortSignal,
	): Promise<string> {
		if (!signal) return record.promise;
		if (signal.aborted && record.phase === "running") {
			this.abortOne(record.id);
			const error = new Error("Aborted");
			error.name = "AbortError";
			return Promise.reject(error);
		}
		return new Promise<string>((resolve, reject) => {
			const cleanup = () => signal.removeEventListener("abort", onAbort);
			const onAbort = () => {
				if (record.phase !== "running") return;
				cleanup();
				this.abortOne(record.id);
				const error = new Error("Aborted");
				error.name = "AbortError";
				reject(error);
			};
			signal.addEventListener("abort", onAbort, { once: true });
			record.promise.then(
				(value) => {
					cleanup();
					resolve(value);
				},
				(error: unknown) => {
					cleanup();
					reject(error);
				},
			);
		});
	}

	private async run(
		record: InternalRecord<Context>,
		config: AgentConfig,
		request: {
			systemPrompt: string;
			prompt: string;
			tools: ToolSet;
			model?: string;
		},
	): Promise<string> {
		let outcome: SubagentRunOutcome;
		try {
			const result = await this.runner({
				...request,
				thinkingLevel: config.thinkingLevel,
				abortSignal: record.abortController.signal,
				context: record.prepared.context,
				drainSteering: () => record.steering.splice(0),
				subscribeSteering: (listener) => {
					record.steeringListeners.add(listener);
					for (const message of record.steering.splice(0)) listener(message);
					return () => record.steeringListeners.delete(listener);
				},
				onMessages: (messages) => {
					record.lastActivityAt = Date.now();
					record.messages = messages;
					this.notify();
				},
				onAgentEvent: (event) => {
					record.lastActivityAt = Date.now();
					this.emit(record, { event: { type: "agent-event", value: event } });
				},
			});
			outcome = record.abortController.signal.aborted
				? { status: "aborted" }
				: { status: "completed", result };
		} catch (error) {
			outcome = aborted(error, record.abortController.signal)
				? { status: "aborted", error: messageOf(error) }
				: { status: "error", error: messageOf(error) };
		}
		// From here onward lifecycle hooks may commit authored state. This phase is
		// irreversible: foreground Stop and session teardown must await it.
		record.phase = "finalizing";
		try {
			const finalizedResult = await record.prepared.finalize?.({
				id: record.id,
				agent: config,
				outcome,
				context: record.prepared.context,
			});
			if (outcome.status === "completed" && finalizedResult !== undefined) {
				outcome = { status: "completed", result: finalizedResult };
			}
		} catch (error) {
			outcome = aborted(error, record.abortController.signal)
				? { status: "aborted", error: messageOf(error) }
				: { status: "error", error: messageOf(error) };
		}

		try {
			await record.prepared.cleanup?.({
				id: record.id,
				agent: config,
				outcome,
				context: record.prepared.context,
			});
		} catch (error) {
			if (outcome.status === "completed") {
				outcome = { status: "error", error: messageOf(error) };
			}
		}

		record.finishedAt = Date.now();
		record.phase = "terminal";
		record.lastActivityAt = record.finishedAt;
		record.status = outcome.status;
		if (outcome.status === "completed") record.result = outcome.result;
		else record.error = outcome.error;
		this.emit(record, { event: { type: "end", ...outcome } });
		this.notify();

		if (outcome.status === "completed") return outcome.result;
		throw outcomeError(outcome);
	}

	private snapshot(record: InternalRecord<Context>): SubagentSnapshot {
		return {
			id: record.id,
			type: record.type,
			status: record.status,
			startedAt: record.startedAt,
			lastActivityAt: record.lastActivityAt,
			finishedAt: record.finishedAt,
			messages: record.messages,
			result: record.result,
			error: record.error,
			parentToolCallId: record.parentToolCallId,
		};
	}

	private notify(): void {
		if (this.listeners.size === 0) return;
		const snapshots = this.list();
		for (const listener of this.listeners) {
			try {
				listener(snapshots);
			} catch (error) {
				console.error("Subagent snapshot listener failed:", error);
			}
		}
	}

	private emit(
		record: InternalRecord<Context>,
		body: Pick<SubagentEvent, "event">,
	): void {
		if (!this.onEvent) return;
		try {
			this.onEvent({
				subagentId: record.id,
				subagentType: record.type,
				parentToolCallId: record.parentToolCallId,
				timestamp: Date.now(),
				...body,
			} as SubagentEvent);
		} catch (error) {
			console.error("Subagent event callback failed:", error);
		}
	}
}
