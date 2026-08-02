/**
 * Makes authority explicit when an async read/write response competes with a
 * newer synchronous projection. A ticket may mutate local state only while it
 * still owns the gate; `supersede()` invalidates every outstanding ticket.
 */
export class LatestRequestGate {
	#epoch = 0;

	issue(): number {
		this.#epoch += 1;
		return this.#epoch;
	}

	supersede(): void {
		this.#epoch += 1;
	}

	owns(ticket: number): boolean {
		return ticket === this.#epoch;
	}
}
