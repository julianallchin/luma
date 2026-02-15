import type { Span } from "./types";

export type DslErrorCode =
	| "unknown_pattern"
	| "unknown_arg"
	| "type_mismatch"
	| "missing_selection"
	| "invalid_bar_range"
	| "invalid_blend_mode"
	| "unexpected_token"
	| "unexpected_eof"
	| "invalid_hex_color"
	| "duplicate_bar_range"
	| "empty_bar_block";

export type DslError = {
	code: DslErrorCode;
	message: string;
	span: Span;
	hint?: string;
};

export type DslWarning = {
	code: string;
	message: string;
	span: Span;
};

export function formatError(error: DslError, source: string): string {
	const lines = source.split("\n");
	const { line, column } = error.span.start;
	const endCol =
		error.span.end.line === line
			? error.span.end.column
			: (lines[line - 1]?.length ?? column + 1);
	const underlineLen = Math.max(1, endCol - column);

	const sourceLine = lines[line - 1] ?? "";
	const gutter = `${line}`;
	const pad = " ".repeat(gutter.length);

	const out = [
		`Error at line ${line}, column ${column + 1}: ${error.message}`,
		`${pad} |`,
		`${gutter} | ${sourceLine}`,
		`${pad} | ${" ".repeat(column)}${"^".repeat(underlineLen)}`,
	];

	if (error.hint) {
		out.push(`${pad} | ${error.hint}`);
	}

	return out.join("\n");
}
