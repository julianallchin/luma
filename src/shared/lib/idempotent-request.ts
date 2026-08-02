export type IdempotentRequest = Readonly<{
	fingerprint: string;
	requestId: string;
}>;

/** Keep an operation ID only while retrying the exact same payload. */
export function idempotentRequestFor(
	previous: IdempotentRequest | null,
	fingerprint: string,
	createId: () => string = crypto.randomUUID,
): IdempotentRequest {
	if (previous?.fingerprint === fingerprint) return previous;
	return { fingerprint, requestId: createId() };
}

/**
 * Owns the retry identity and single-flight invariant for one idempotent UI
 * action. A failed attempt keeps its request ID for an identical retry; a
 * successful attempt consumes it. Settling an attempt after `reset()` is a
 * no-op, so stale async responses cannot act on a newly loaded subject.
 */
export class IdempotentRequestGate {
	#retry: IdempotentRequest | null = null;
	#inFlight: IdempotentRequest | null = null;

	constructor(private readonly createId: () => string = crypto.randomUUID) {}

	begin(fingerprint: string): IdempotentRequest | null {
		if (this.#inFlight) return null;
		this.#retry = idempotentRequestFor(this.#retry, fingerprint, this.createId);
		this.#inFlight = this.#retry;
		return this.#inFlight;
	}

	succeed(request: IdempotentRequest): boolean {
		if (this.#inFlight !== request) return false;
		this.#inFlight = null;
		this.#retry = null;
		return true;
	}

	fail(request: IdempotentRequest): boolean {
		if (this.#inFlight !== request) return false;
		this.#inFlight = null;
		return true;
	}

	reset(): void {
		this.#inFlight = null;
		this.#retry = null;
	}
}
