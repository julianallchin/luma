import type { UIMessageChunk } from "ai";

type DeltaChunk = Extract<
	UIMessageChunk,
	{ type: "text-delta" | "reasoning-delta" }
>;

type Segment =
	| { kind: "delta"; chunk: DeltaChunk; body: string; position: number }
	| { kind: "event"; chunk: UIMessageChunk }
	| { kind: "end" };

type SmoothOptions = {
	windowMs?: number;
	minCharsPerSecond?: number;
	maxCharsPerSecond?: number;
};

const RATE_EMA = 0.25;

function isDelta(chunk: UIMessageChunk): chunk is DeltaChunk {
	return chunk.type === "text-delta" || chunk.type === "reasoning-delta";
}

function isSpace(character: string): boolean {
	return (
		character === " " ||
		character === "\n" ||
		character === "\t" ||
		character === "\r"
	);
}

/** End of the next whole word, including adjacent whitespace. */
function nextWordEnd(body: string, position: number, limit: number): number {
	let index = position;
	while (index < limit && isSpace(body[index] ?? "")) index += 1;
	while (index < limit && !isSpace(body[index] ?? "")) index += 1;
	while (index < limit && isSpace(body[index] ?? "")) index += 1;
	return index;
}

/**
 * Smooth provider-sized text/reasoning bursts into adaptive word-sized chunks.
 * Non-text chunks retain their order and are never sliced.
 */
export function smoothUIMessageStream(
	source: ReadableStream<UIMessageChunk>,
	options: SmoothOptions = {},
): ReadableStream<UIMessageChunk> {
	const windowSeconds = (options.windowMs ?? 400) / 1000;
	const minimumRate = options.minCharsPerSecond ?? 40;
	const maximumRate = options.maxCharsPerSecond ?? 900;
	let reader: ReadableStreamDefaultReader<UIMessageChunk> | null = null;
	let dispose: (() => void) | null = null;

	return new ReadableStream<UIMessageChunk>({
		start(controller) {
			const queue: Segment[] = [];
			let frame = 0;
			let lastFrame = 0;
			let carry = 0;
			let rate = minimumRate;
			let closed = false;

			const backlog = () =>
				queue.reduce(
					(total, segment) =>
						segment.kind === "delta"
							? total + segment.body.length - segment.position
							: total,
					0,
				);

			const emitRemainder = (segment: Extract<Segment, { kind: "delta" }>) => {
				if (segment.position >= segment.body.length) return;
				controller.enqueue({
					...segment.chunk,
					delta: segment.body.slice(segment.position),
				});
			};

			const resetPacing = () => {
				lastFrame = 0;
				carry = 0;
				rate = minimumRate;
			};

			const dumpQueue = () => {
				if (frame) cancelAnimationFrame(frame);
				frame = 0;
				for (const segment of queue) {
					if (segment.kind === "delta") emitRemainder(segment);
					else if (segment.kind === "event") controller.enqueue(segment.chunk);
					else if (!closed) {
						closed = true;
						controller.close();
					}
				}
				queue.length = 0;
				resetPacing();
			};

			const schedule = () => {
				if (!frame && !closed) frame = requestAnimationFrame(tick);
			};

			const tick = (timestamp: number) => {
				frame = 0;
				const elapsed =
					lastFrame === 0 ? 16 : Math.min(timestamp - lastFrame, 100);
				lastFrame = timestamp;
				const target = Math.min(
					maximumRate,
					Math.max(minimumRate, backlog() / windowSeconds),
				);
				rate += (target - rate) * RATE_EMA;
				carry += (rate * elapsed) / 1000;

				while (queue.length > 0) {
					const head = queue[0];
					if (!head) break;
					if (head.kind === "event") {
						controller.enqueue(head.chunk);
						queue.shift();
						continue;
					}
					if (head.kind === "end") {
						queue.shift();
						closed = true;
						controller.close();
						break;
					}

					// Hold an unfinished trailing word until the next provider delta
					// establishes its boundary.
					const sealed = queue.length > 1;
					const limit = sealed
						? head.body.length
						: Math.max(
								head.position,
								head.body.lastIndexOf(" ") + 1,
								head.body.lastIndexOf("\n") + 1,
							);
					if (head.position >= limit) {
						if (sealed && head.position >= head.body.length) {
							queue.shift();
							continue;
						}
						break;
					}

					const end = nextWordEnd(head.body, head.position, limit);
					const size = end - head.position;
					if (size <= 0 || size > carry) break;
					controller.enqueue({
						...head.chunk,
						delta: head.body.slice(head.position, end),
					});
					head.position = end;
					carry -= size;
					if (sealed && head.position >= head.body.length) queue.shift();
				}

				if (queue.length > 0 && !closed) schedule();
				else resetPacing();
			};

			const push = (chunk: UIMessageChunk) => {
				if (closed) return;
				if (document.visibilityState === "hidden") {
					dumpQueue();
					controller.enqueue(chunk);
					return;
				}
				if (!isDelta(chunk)) {
					queue.push({ kind: "event", chunk });
					schedule();
					return;
				}
				const tail = queue.at(-1);
				if (
					tail?.kind === "delta" &&
					tail.chunk.type === chunk.type &&
					tail.chunk.id === chunk.id
				) {
					tail.body += chunk.delta;
				} else {
					queue.push({
						kind: "delta",
						chunk,
						body: chunk.delta,
						position: 0,
					});
				}
				schedule();
			};

			const onVisibilityChange = () => {
				if (document.visibilityState === "visible") dumpQueue();
			};
			document.addEventListener("visibilitychange", onVisibilityChange);

			dispose = () => {
				document.removeEventListener("visibilitychange", onVisibilityChange);
				if (frame) cancelAnimationFrame(frame);
				frame = 0;
				queue.length = 0;
			};

			const sourceReader = source.getReader();
			reader = sourceReader;
			void (async () => {
				try {
					while (true) {
						const { done, value } = await sourceReader.read();
						if (done) {
							queue.push({ kind: "end" });
							schedule();
							break;
						}
						if (value) push(value);
					}
				} catch (error) {
					dispose?.();
					if (!closed) controller.error(error);
				}
			})();
		},
		cancel(reason) {
			dispose?.();
			return reader?.cancel(reason);
		},
	});
}
