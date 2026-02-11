import { describe, expect, it } from "vitest";
import { tokenize } from "../tokenizer";

describe("tokenizer", () => {
	it("tokenizes a bar header", () => {
		const tokens = tokenize("@1-4");
		expect(tokens.map((t) => t.type)).toEqual([
			"at",
			"number",
			"dash",
			"number",
			"eof",
		]);
		expect(tokens[1].value).toBe("1");
		expect(tokens[3].value).toBe("4");
	});

	it("tokenizes a single bar header", () => {
		const tokens = tokenize("@9");
		expect(tokens.map((t) => t.type)).toEqual(["at", "number", "eof"]);
	});

	it("tokenizes a pattern layer with args", () => {
		const tokens = tokenize("solid_color(all) color=#ff0000");
		expect(tokens.map((t) => t.type)).toEqual([
			"identifier",
			"lparen",
			"identifier",
			"rparen",
			"identifier",
			"equals",
			"hex_color",
			"eof",
		]);
		expect(tokens[0].value).toBe("solid_color");
		expect(tokens[2].value).toBe("all");
		expect(tokens[6].value).toBe("#ff0000");
	});

	it("tokenizes numeric args", () => {
		const tokens = tokenize("subdivision=2 max_dimmer=0.8");
		expect(tokens.map((t) => t.type)).toEqual([
			"identifier",
			"equals",
			"number",
			"identifier",
			"equals",
			"number",
			"eof",
		]);
		expect(tokens[2].value).toBe("2");
		expect(tokens[5].value).toBe("0.8");
	});

	it("tokenizes comments", () => {
		const tokens = tokenize("# Intro — ambient wash");
		expect(tokens).toHaveLength(2); // comment + eof
		expect(tokens[0].type).toBe("comment");
		expect(tokens[0].value).toBe("Intro — ambient wash");
	});

	it("distinguishes hex color from comment by position", () => {
		const tokens = tokenize("color=#aabbcc # this is a comment");
		expect(tokens.map((t) => t.type)).toEqual([
			"identifier",
			"equals",
			"hex_color",
			"comment",
			"eof",
		]);
		expect(tokens[2].value).toBe("#aabbcc");
		expect(tokens[3].type).toBe("comment");
	});

	it("tokenizes tag expression operators", () => {
		const tokens = tokenize("(left & wash | ~high > accent ^ hit)");
		const types = tokens.map((t) => t.type);
		expect(types).toEqual([
			"lparen",
			"identifier",
			"and",
			"identifier",
			"or",
			"not",
			"identifier",
			"fallback",
			"identifier",
			"xor",
			"identifier",
			"rparen",
			"eof",
		]);
	});

	it("tokenizes hold keyword", () => {
		const tokens = tokenize("hold");
		expect(tokens[0].type).toBe("identifier");
		expect(tokens[0].value).toBe("hold");
	});

	it("preserves newlines as tokens", () => {
		const tokens = tokenize("@1\nsolid_color(all)\n");
		const types = tokens.map((t) => t.type);
		expect(types).toEqual([
			"at",
			"number",
			"newline",
			"identifier",
			"lparen",
			"identifier",
			"rparen",
			"newline",
			"eof",
		]);
	});

	it("tracks line and column correctly", () => {
		const tokens = tokenize("@1\nsolid_color(all)");
		const solidToken = tokens.find((t) => t.value === "solid_color");
		expect(solidToken?.span.start.line).toBe(2);
		expect(solidToken?.span.start.column).toBe(0);
	});

	it("handles blend= identifier arg", () => {
		const tokens = tokenize("blend=add");
		expect(tokens.map((t) => t.type)).toEqual([
			"identifier",
			"equals",
			"identifier",
			"eof",
		]);
	});

	it("handles empty input", () => {
		const tokens = tokenize("");
		expect(tokens).toHaveLength(1);
		expect(tokens[0].type).toBe("eof");
	});

	it("handles CRLF line endings", () => {
		const tokens = tokenize("@1\r\n@2");
		const types = tokens.map((t) => t.type);
		expect(types).toEqual(["at", "number", "newline", "at", "number", "eof"]);
	});

	it("tokenizes uppercase hex colors", () => {
		const tokens = tokenize("color=#AABBCC");
		expect(tokens[2].type).toBe("hex_color");
		expect(tokens[2].value).toBe("#AABBCC");
	});
});
