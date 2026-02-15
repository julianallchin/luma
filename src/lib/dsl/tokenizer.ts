import type { Loc, Span } from "./types";

export type TokenType =
	| "at" // @
	| "dash" // -
	| "lparen" // (
	| "rparen" // )
	| "equals" // =
	| "and" // &
	| "or" // |
	| "xor" // ^
	| "not" // ~
	| "fallback" // >
	| "hex_color" // #rrggbb
	| "number" // integer or float
	| "identifier" // word
	| "newline" // \n
	| "comment" // # text
	| "eof";

export type Token = {
	type: TokenType;
	value: string;
	span: Span;
};

function loc(line: number, column: number, offset: number): Loc {
	return { line, column, offset };
}

function span(start: Loc, end: Loc): Span {
	return { start, end };
}

const HEX_RE = /^[0-9a-fA-F]{6}$/;

export function tokenize(source: string): Token[] {
	const tokens: Token[] = [];
	let pos = 0;
	let line = 1;
	let col = 0;

	function peek(): string {
		return pos < source.length ? source[pos] : "";
	}

	function advance(): string {
		const ch = source[pos];
		pos++;
		col++;
		return ch;
	}

	function current(): Loc {
		return loc(line, col, pos);
	}

	function skipInlineWhitespace(): void {
		while (
			pos < source.length &&
			(source[pos] === " " || source[pos] === "\t")
		) {
			advance();
		}
	}

	while (pos < source.length) {
		skipInlineWhitespace();

		if (pos >= source.length) break;

		const ch = peek();
		const start = current();

		if (ch === "\n" || ch === "\r") {
			advance();
			if (ch === "\r" && peek() === "\n") advance();
			tokens.push({
				type: "newline",
				value: "\n",
				span: span(start, current()),
			});
			line++;
			col = 0;
			continue;
		}

		if (ch === "#") {
			// Check if this is a hex color: # followed by exactly 6 hex digits
			// This only happens when the previous token is "=" (value position)
			const remaining = source.slice(pos + 1, pos + 7);
			if (
				HEX_RE.test(remaining) &&
				tokens.length > 0 &&
				tokens[tokens.length - 1].type === "equals"
			) {
				advance(); // skip #
				let hex = "";
				for (let i = 0; i < 6; i++) {
					hex += advance();
				}
				tokens.push({
					type: "hex_color",
					value: `#${hex}`,
					span: span(start, current()),
				});
				continue;
			}

			// Otherwise it's a comment — consume rest of line
			let text = "";
			advance(); // skip #
			while (
				pos < source.length &&
				source[pos] !== "\n" &&
				source[pos] !== "\r"
			) {
				text += advance();
			}
			tokens.push({
				type: "comment",
				value: text.trim(),
				span: span(start, current()),
			});
			continue;
		}

		if (ch === "@") {
			advance();
			tokens.push({ type: "at", value: "@", span: span(start, current()) });
			continue;
		}

		if (ch === "-") {
			advance();
			tokens.push({ type: "dash", value: "-", span: span(start, current()) });
			continue;
		}

		if (ch === "(") {
			advance();
			tokens.push({ type: "lparen", value: "(", span: span(start, current()) });
			continue;
		}

		if (ch === ")") {
			advance();
			tokens.push({ type: "rparen", value: ")", span: span(start, current()) });
			continue;
		}

		if (ch === "=") {
			advance();
			tokens.push({ type: "equals", value: "=", span: span(start, current()) });
			continue;
		}

		if (ch === "&") {
			advance();
			tokens.push({ type: "and", value: "&", span: span(start, current()) });
			continue;
		}

		if (ch === "|") {
			advance();
			tokens.push({ type: "or", value: "|", span: span(start, current()) });
			continue;
		}

		if (ch === "^") {
			advance();
			tokens.push({ type: "xor", value: "^", span: span(start, current()) });
			continue;
		}

		if (ch === "~") {
			advance();
			tokens.push({ type: "not", value: "~", span: span(start, current()) });
			continue;
		}

		if (ch === ">") {
			advance();
			tokens.push({
				type: "fallback",
				value: ">",
				span: span(start, current()),
			});
			continue;
		}

		// Numbers: integers and floats (including negative via preceding dash token)
		if (ch >= "0" && ch <= "9") {
			let num = "";
			while (pos < source.length && source[pos] >= "0" && source[pos] <= "9") {
				num += advance();
			}
			if (pos < source.length && source[pos] === ".") {
				num += advance();
				while (
					pos < source.length &&
					source[pos] >= "0" &&
					source[pos] <= "9"
				) {
					num += advance();
				}
			}
			tokens.push({
				type: "number",
				value: num,
				span: span(start, current()),
			});
			continue;
		}

		// Identifiers: [a-zA-Z_][a-zA-Z0-9_]*
		if ((ch >= "a" && ch <= "z") || (ch >= "A" && ch <= "Z") || ch === "_") {
			let ident = "";
			while (
				pos < source.length &&
				((source[pos] >= "a" && source[pos] <= "z") ||
					(source[pos] >= "A" && source[pos] <= "Z") ||
					(source[pos] >= "0" && source[pos] <= "9") ||
					source[pos] === "_")
			) {
				ident += advance();
			}
			tokens.push({
				type: "identifier",
				value: ident,
				span: span(start, current()),
			});
			continue;
		}

		// Unknown character — skip it (parser will handle errors)
		advance();
	}

	const endLoc = current();
	tokens.push({ type: "eof", value: "", span: span(endLoc, endLoc) });
	return tokens;
}
