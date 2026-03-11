import type { DslError, DslWarning } from "./errors";
import type { Token } from "./tokenizer";
import { tokenize } from "./tokenizer";
import {
	type Annotation,
	type Arg,
	type ArgValue,
	type BarRange,
	BLEND_MODES,
	type BlendMode,
	DEFAULT_BLEND_MODE,
	type Document,
	type GroupExpr,
	type Loc,
	type PatternRegistry,
	type Span,
} from "./types";

export type ParseOptions = {
	/** Beats per bar for interpreting bar:beat:sub notation. Default: 4 */
	beatsPerBar?: number;
	/** Subdivisions per beat for interpreting bar:beat:sub notation. Default: 2 (eighth notes) */
	subsPerBeat?: number;
};

export type ParseResult =
	| { ok: true; document: Document; warnings: DslWarning[] }
	| { ok: false; errors: DslError[]; partial: Document | null };

export function parse(
	source: string,
	registry: PatternRegistry,
	options?: ParseOptions,
): ParseResult {
	const tokens = tokenize(source);
	const parser = new Parser(tokens, registry, source, options);
	return parser.parse();
}

class Parser {
	private pos = 0;
	private errors: DslError[] = [];
	private warnings: DslWarning[] = [];
	private beatsPerBar: number;
	private subsPerBeat: number;

	constructor(
		private tokens: Token[],
		private registry: PatternRegistry,
		_source: string,
		options?: ParseOptions,
	) {
		this.beatsPerBar = options?.beatsPerBar ?? 4;
		this.subsPerBeat = options?.subsPerBeat ?? 4;
	}

	parse(): ParseResult {
		// Parse annotations grouped into layers (separated by blank lines)
		const layers: Annotation[][] = [];
		let currentLayer: Annotation[] = [];

		while (!this.isAtEnd()) {
			this.skipComments();
			if (this.isAtEnd()) break;

			// Blank line(s) → start a new layer (if current has content)
			if (this.check("newline")) {
				// Count consecutive newlines (blank line = layer separator)
				let newlineCount = 0;
				while (this.check("newline")) {
					this.advance();
					newlineCount++;
					this.skipComments();
				}
				// 2+ newlines (i.e. at least one blank line) → layer break
				if (newlineCount >= 2 && currentLayer.length > 0) {
					layers.push(currentLayer);
					currentLayer = [];
				}
				continue;
			}

			if (this.check("identifier")) {
				const annotation = this.parseAnnotation();
				if (annotation) currentLayer.push(annotation);
			} else {
				this.addError(
					"unexpected_token",
					`expected pattern name, got "${this.peek().value}"`,
					this.peek().span,
				);
				this.skipToNextLine();
			}
		}

		// Push the last layer
		if (currentLayer.length > 0) {
			layers.push(currentLayer);
		}

		const doc: Document = { layers };

		// Check for overlapping annotations within each layer
		for (let li = 0; li < layers.length; li++) {
			const layer = layers[li];
			for (let i = 1; i < layer.length; i++) {
				const prev = layer[i - 1];
				const curr = layer[i];
				if (curr.range.start < prev.range.end) {
					this.warnings.push({
						code: "overlap",
						message: `"${curr.pattern}" @${curr.range.start} overlaps with "${prev.pattern}" @${prev.range.start}-${prev.range.end} in layer ${li}`,
						span: curr.span,
					});
				}
			}
		}

		if (this.errors.length > 0) {
			return { ok: false, errors: this.errors, partial: doc };
		}
		return { ok: true, document: doc, warnings: this.warnings };
	}

	// ── Annotation parsing ────────────────────────────────────────

	private parseAnnotation(): Annotation | null {
		const nameTok = this.advance();
		const patternName = nameTok.value;
		const start = nameTok.span.start;

		// Validate pattern name
		if (!this.registry.has(patternName)) {
			const available = [...this.registry.keys()].join(", ");
			this.addError(
				"unknown_pattern",
				`unknown pattern "${patternName}"`,
				nameTok.span,
				{
					hint: `Available patterns: ${available}`,
				},
			);
			this.skipToNextLine();
			return null;
		}

		// Parse selection expression in parentheses
		if (!this.check("lparen")) {
			this.addError(
				"missing_selection",
				`expected "(" after pattern name "${patternName}"`,
				this.peek().span,
			);
			this.skipToNextLine();
			return null;
		}
		this.advance(); // consume (
		const selection = this.parseGroupExpr();
		if (!this.check("rparen")) {
			this.addError("unexpected_token", 'expected ")"', this.peek().span);
			this.skipToNextLine();
			return null;
		}
		this.advance(); // consume )

		// Parse bar range (@start-end or @start) — required
		let range: BarRange | null = null;
		if (this.check("at")) {
			range = this.parseBarRange();
		}

		if (!range) {
			this.addError(
				"unexpected_token",
				`expected bar range (@) for annotation "${patternName}"`,
				this.peek().span,
			);
			this.skipToNextLine();
			return null;
		}

		// Parse args on the same line
		const args: Arg[] = [];
		let blend: BlendMode = DEFAULT_BLEND_MODE;

		while (
			!this.isAtEnd() &&
			!this.check("newline") &&
			!this.check("comment")
		) {
			if (!this.check("identifier")) break;

			const keyTok = this.peek();
			// Look ahead for = sign
			if (
				this.pos + 1 < this.tokens.length &&
				this.tokens[this.pos + 1].type === "equals"
			) {
				this.advance(); // consume key
				this.advance(); // consume =
				const key = keyTok.value;

				if (key === "blend") {
					const valTok = this.peek();
					if (valTok.type === "identifier") {
						this.advance();
						if (BLEND_MODES.includes(valTok.value as BlendMode)) {
							blend = valTok.value as BlendMode;
						} else {
							this.addError(
								"invalid_blend_mode",
								`invalid blend mode "${valTok.value}"`,
								valTok.span,
								{ hint: `Valid modes: ${BLEND_MODES.join(", ")}` },
							);
						}
					} else {
						this.addError(
							"unexpected_token",
							"expected blend mode identifier",
							valTok.span,
						);
					}
					continue;
				}

				const value = this.parseArgValue();
				if (!value) continue;

				const argSpan: Span = {
					start: keyTok.span.start,
					end: value.span.end,
				};

				// Validate arg key against pattern definition
				const patternDef = this.registry.get(patternName);
				if (!patternDef) continue;
				const argDef = patternDef.args.find(
					(a) => a.name === key && a.argType !== "Selection",
				);

				if (!argDef) {
					const isSelectionArg = patternDef.args.find(
						(a) => a.name === key && a.argType === "Selection",
					);
					if (isSelectionArg) {
						this.warnings.push({
							code: "selection_as_arg",
							message: `"${key}" is a Selection arg — use the parenthesized selection instead`,
							span: argSpan,
						});
					} else if (!patternDef.args.find((a) => a.name === key)) {
						this.warnings.push({
							code: "unknown_arg",
							message: `unknown arg "${key}" for pattern "${patternName}"`,
							span: argSpan,
						});
					}
				} else {
					this.validateArgType(argDef.argType, value.value, key, argSpan);
				}

				args.push({ key, value: value.value, span: argSpan });
			} else {
				break;
			}
		}

		// Skip trailing comment on the same line
		if (this.check("comment")) {
			this.advance();
		}

		const end = this.prevEnd();

		return {
			type: "annotation",
			pattern: patternName,
			selection,
			range,
			args,
			blend,
			span: { start, end },
		};
	}

	// ── Bar range ─────────────────────────────────────────────────
	// Formats:
	//   @5        → single bar [5, 6)
	//   @5-8      → bars [5, 8)
	//   @5:3      → bar 5 beat 3 to bar 5 beat 4 [5.5, 5.75) in 4/4
	//   @5:3-5:4  → bar 5 beat 3 to beat 4
	//   @5:3:2    → bar 5 beat 3 second half
	//   @5:3:2-6  → bar 5 beat 3 second half to bar 6

	private parseBarRange(): BarRange | null {
		this.advance(); // consume @

		const startResult = this.parseBarPosition();
		if (!startResult) return null;

		const rangeStart = startResult.value;
		let rangeEnd = rangeStart + 1; // default: single bar
		let spanEnd = startResult.spanEnd;

		// If start had beat/sub components, default end is one unit past start
		if (startResult.hasBeats && !startResult.hasSubs) {
			// @5:3 → one beat: [5:3, 5:4) = [5.5, 5.75) in 4/4
			rangeEnd = rangeStart + 1 / this.beatsPerBar;
		} else if (startResult.hasSubs) {
			// @5:3:2 → one subdivision: [5:3:2, next sub)
			rangeEnd = rangeStart + 1 / (this.beatsPerBar * this.subsPerBeat);
		}

		if (this.check("dash")) {
			this.advance(); // consume -
			const endResult = this.parseBarPosition();
			if (!endResult) return null;
			rangeEnd = endResult.value;
			spanEnd = endResult.spanEnd;
		}

		if (rangeEnd < rangeStart) {
			this.addError("invalid_bar_range", `bar range end must be > start`, {
				start: startResult.spanStart,
				end: spanEnd,
			});
			return null;
		}
		if (rangeEnd <= rangeStart) {
			// Zero-length range: clamp to minimum 1 subdivision
			rangeEnd = rangeStart + 1 / (this.beatsPerBar * this.subsPerBeat);
		}

		return { start: rangeStart, end: rangeEnd };
	}

	/**
	 * Parse a bar position: bar or bar:beat or bar:beat:sub
	 * Returns the fractional bar number.
	 */
	private parseBarPosition(): {
		value: number;
		hasBeats: boolean;
		hasSubs: boolean;
		spanStart: Loc;
		spanEnd: Loc;
	} | null {
		const barTok = this.expect("number");
		if (!barTok) return null;
		const bar = Number.parseInt(barTok.value, 10);
		let value = bar;
		let hasBeats = false;
		let hasSubs = false;
		let spanEnd = barTok.span.end;

		if (this.check("colon")) {
			this.advance(); // consume :
			const beatTok = this.expect("number");
			if (!beatTok) return null;
			const beat = Number.parseInt(beatTok.value, 10);
			hasBeats = true;
			spanEnd = beatTok.span.end;
			// beat is 1-indexed: beat 1 = start of bar, beat N = (N-1)/beatsPerBar
			value = bar + (beat - 1) / this.beatsPerBar;

			if (this.check("colon")) {
				this.advance(); // consume :
				const subTok = this.expect("number");
				if (!subTok) return null;
				const sub = Number.parseInt(subTok.value, 10);
				hasSubs = true;
				spanEnd = subTok.span.end;
				// sub is 1-indexed: sub 1 = start of beat, sub 2 = halfway through
				value =
					bar +
					(beat - 1) / this.beatsPerBar +
					(sub - 1) / (this.beatsPerBar * this.subsPerBeat);
			}
		}

		return {
			value,
			hasBeats,
			hasSubs,
			spanStart: barTok.span.start,
			spanEnd,
		};
	}

	private parseArgValue(): { value: ArgValue; span: Span } | null {
		const tok = this.peek();

		if (tok.type === "hex_color") {
			this.advance();
			return {
				value: { type: "color", hex: tok.value },
				span: tok.span,
			};
		}

		if (tok.type === "number") {
			this.advance();
			return {
				value: { type: "number", value: Number.parseFloat(tok.value) },
				span: tok.span,
			};
		}

		if (tok.type === "identifier") {
			this.advance();
			return {
				value: { type: "identifier", value: tok.value },
				span: tok.span,
			};
		}

		this.addError(
			"unexpected_token",
			`expected value, got "${tok.value}"`,
			tok.span,
		);
		return null;
	}

	private validateArgType(
		expected: string,
		value: ArgValue,
		key: string,
		span: Span,
	): void {
		if (expected === "Color" && value.type !== "color") {
			this.addError(
				"type_mismatch",
				`expected color for arg "${key}", got ${value.type}`,
				span,
			);
		}
		if (expected === "Scalar" && value.type !== "number") {
			this.addError(
				"type_mismatch",
				`expected number for arg "${key}", got ${value.type}`,
				span,
			);
		}
	}

	// ── Group expressions ────────────────────────────────────────────
	// Precedence (lowest to highest): > | ^ & ~

	private parseGroupExpr(): GroupExpr {
		return this.parseFallback();
	}

	private parseFallback(): GroupExpr {
		let left = this.parseOr();
		while (this.check("fallback")) {
			this.advance();
			const right = this.parseOr();
			left = { type: "fallback", left, right };
		}
		return left;
	}

	private parseOr(): GroupExpr {
		let left = this.parseXor();
		while (this.check("or")) {
			this.advance();
			const right = this.parseXor();
			left = { type: "or", left, right };
		}
		return left;
	}

	private parseXor(): GroupExpr {
		let left = this.parseAnd();
		while (this.check("xor")) {
			this.advance();
			const right = this.parseAnd();
			left = { type: "xor", left, right };
		}
		return left;
	}

	private parseAnd(): GroupExpr {
		let left = this.parseUnary();
		while (this.check("and")) {
			this.advance();
			const right = this.parseUnary();
			left = { type: "and", left, right };
		}
		return left;
	}

	private parseUnary(): GroupExpr {
		if (this.check("not")) {
			this.advance();
			const operand = this.parseUnary();
			return { type: "not", operand };
		}
		return this.parsePrimary();
	}

	private parsePrimary(): GroupExpr {
		if (this.check("lparen")) {
			this.advance();
			const inner = this.parseGroupExpr();
			if (this.check("rparen")) {
				this.advance();
			}
			return { type: "paren", inner };
		}

		if (this.check("identifier")) {
			const tok = this.advance();
			return { type: "group", name: tok.value };
		}

		// Error recovery: return a placeholder
		const tok = this.peek();
		this.addError(
			"unexpected_token",
			`expected group name, got "${tok.value}"`,
			tok.span,
		);
		return { type: "group", name: "all" };
	}

	// ── Helpers ────────────────────────────────────────────────────

	private peek(): Token {
		return this.tokens[this.pos];
	}

	private advance(): Token {
		const tok = this.tokens[this.pos];
		if (!this.isAtEnd()) this.pos++;
		return tok;
	}

	private check(type: Token["type"]): boolean {
		return this.peek().type === type;
	}

	private isAtEnd(): boolean {
		return this.peek().type === "eof";
	}

	private expect(type: Token["type"]): Token | null {
		if (this.check(type)) {
			return this.advance();
		}
		this.addError(
			"unexpected_token",
			`expected ${type}, got "${this.peek().value}"`,
			this.peek().span,
		);
		return null;
	}

	private skipComments(): void {
		while (this.check("comment")) {
			this.advance();
			// Comments end at newline — consume the newline too
			if (this.check("newline")) {
				this.advance();
			}
		}
	}

	private skipToNextLine(): void {
		while (!this.isAtEnd() && !this.check("newline")) {
			this.advance();
		}
		if (this.check("newline")) this.advance();
	}

	private prevEnd(): Loc {
		if (this.pos > 0) {
			return this.tokens[this.pos - 1].span.end;
		}
		return { line: 1, column: 0, offset: 0 };
	}

	private addError(
		code: DslError["code"],
		message: string,
		span: Span,
		opts?: { hint?: string },
	): void {
		this.errors.push({ code, message, span, hint: opts?.hint });
	}
}
