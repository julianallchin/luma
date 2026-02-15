import type { DslError, DslWarning } from "./errors";
import type { Token } from "./tokenizer";
import { tokenize } from "./tokenizer";
import {
	type Arg,
	type ArgValue,
	type BarBlock,
	BLEND_MODES,
	type BlendMode,
	DEFAULT_BLEND_MODE,
	type Document,
	type Layer,
	type Loc,
	type PatternRegistry,
	type Span,
	type TagExpr,
} from "./types";

export type ParseResult =
	| { ok: true; document: Document; warnings: DslWarning[] }
	| { ok: false; errors: DslError[]; partial: Document | null };

export function parse(source: string, registry: PatternRegistry): ParseResult {
	const tokens = tokenize(source);
	const parser = new Parser(tokens, registry, source);
	return parser.parse();
}

class Parser {
	private pos = 0;
	private errors: DslError[] = [];
	private warnings: DslWarning[] = [];

	constructor(
		private tokens: Token[],
		private registry: PatternRegistry,
		_source: string,
	) {}

	parse(): ParseResult {
		const bars: BarBlock[] = [];

		while (!this.isAtEnd()) {
			this.skipBlanksAndComments();
			if (this.isAtEnd()) break;

			if (this.check("at")) {
				const block = this.parseBarBlock();
				if (block) bars.push(block);
			} else {
				// Unexpected token at top level — skip to next line
				this.addError(
					"unexpected_token",
					`expected bar header (@), got "${this.peek().value}"`,
					this.peek().span,
				);
				this.skipToNextLine();
			}
		}

		const doc: Document = { bars };

		if (this.errors.length > 0) {
			return { ok: false, errors: this.errors, partial: doc };
		}
		return { ok: true, document: doc, warnings: this.warnings };
	}

	// ── Bar blocks ─────────────────────────────────────────────────

	private parseBarBlock(): BarBlock | null {
		const start = this.peek().span.start;
		const range = this.parseBarHeader();
		if (!range) return null;

		this.expectNewline();

		const layers: Layer[] = [];
		while (!this.isAtEnd() && !this.check("at")) {
			this.skipBlanksAndComments();
			if (this.isAtEnd() || this.check("at")) break;

			const layer = this.parseLayer();
			if (layer) layers.push(layer);
		}

		const end =
			layers.length > 0 ? layers[layers.length - 1].span.end : range.spanEnd;

		return {
			range: { start: range.start, end: range.end },
			layers,
			span: { start, end },
		};
	}

	private parseBarHeader(): {
		start: number;
		end: number;
		spanEnd: Loc;
	} | null {
		this.expect("at"); // consume @

		const startTok = this.expect("number");
		if (!startTok) return null;
		const start = Number.parseInt(startTok.value, 10);

		let end = start;
		let spanEnd = startTok.span.end;

		if (this.check("dash")) {
			this.advance(); // consume -
			const endTok = this.expect("number");
			if (!endTok) return null;
			end = Number.parseInt(endTok.value, 10);
			spanEnd = endTok.span.end;
		}

		if (end < start) {
			this.addError(
				"invalid_bar_range",
				`bar range end (${end}) must be >= start (${start})`,
				{ start: startTok.span.start, end: spanEnd },
			);
			return null;
		}

		return { start, end, spanEnd };
	}

	// ── Layers ─────────────────────────────────────────────────────

	private parseLayer(): Layer | null {
		const tok = this.peek();

		if (tok.type === "identifier" && tok.value === "hold") {
			this.advance();
			const span = tok.span;
			this.skipToNextLine();
			return { type: "hold", span };
		}

		if (tok.type === "identifier") {
			return this.parsePatternLayer();
		}

		this.addError(
			"unexpected_token",
			`expected pattern name or "hold", got "${tok.value}"`,
			tok.span,
		);
		this.skipToNextLine();
		return null;
	}

	private parsePatternLayer(): Layer | null {
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
		const selection = this.parseTagExpr();
		if (!this.check("rparen")) {
			this.addError("unexpected_token", 'expected ")"', this.peek().span);
			this.skipToNextLine();
			return null;
		}
		this.advance(); // consume )

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
					// Check if it's a selection arg (those are set via parentheses)
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
					// Type-check the value
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
			type: "pattern",
			pattern: patternName,
			selection,
			args,
			blend,
			span: { start, end },
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

	// ── Tag expressions ────────────────────────────────────────────
	// Precedence (lowest to highest): > | ^ & ~

	private parseTagExpr(): TagExpr {
		return this.parseFallback();
	}

	private parseFallback(): TagExpr {
		let left = this.parseOr();
		while (this.check("fallback")) {
			this.advance();
			const right = this.parseOr();
			left = { type: "fallback", left, right };
		}
		return left;
	}

	private parseOr(): TagExpr {
		let left = this.parseXor();
		while (this.check("or")) {
			this.advance();
			const right = this.parseXor();
			left = { type: "or", left, right };
		}
		return left;
	}

	private parseXor(): TagExpr {
		let left = this.parseAnd();
		while (this.check("xor")) {
			this.advance();
			const right = this.parseAnd();
			left = { type: "xor", left, right };
		}
		return left;
	}

	private parseAnd(): TagExpr {
		let left = this.parseUnary();
		while (this.check("and")) {
			this.advance();
			const right = this.parseUnary();
			left = { type: "and", left, right };
		}
		return left;
	}

	private parseUnary(): TagExpr {
		if (this.check("not")) {
			this.advance();
			const operand = this.parseUnary();
			return { type: "not", operand };
		}
		return this.parsePrimary();
	}

	private parsePrimary(): TagExpr {
		if (this.check("lparen")) {
			this.advance();
			const inner = this.parseTagExpr();
			if (this.check("rparen")) {
				this.advance();
			}
			return { type: "group", inner };
		}

		if (this.check("identifier")) {
			const tok = this.advance();
			return { type: "tag", name: tok.value };
		}

		// Error recovery: return a placeholder tag
		const tok = this.peek();
		this.addError(
			"unexpected_token",
			`expected tag name, got "${tok.value}"`,
			tok.span,
		);
		return { type: "tag", name: "all" };
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

	private expectNewline(): void {
		// Consume newline or comment+newline
		if (this.check("comment")) this.advance();
		if (this.check("newline")) {
			this.advance();
		} else if (!this.isAtEnd()) {
			// Don't error on EOF — it's fine if the last bar header is at EOF
		}
	}

	private skipBlanksAndComments(): void {
		while (this.check("newline") || this.check("comment")) {
			this.advance();
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
