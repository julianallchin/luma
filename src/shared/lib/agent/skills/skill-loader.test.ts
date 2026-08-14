import { describe, expect, it } from "vitest";
import { BUNDLED_SKILL_SOURCES } from "./bundled-skills";
import {
	buildSkillTool,
	SkillLoader,
	skillToolDescription,
} from "./skill-loader";

const RAW = `---
name: test-genre
description: A test genre playbook.
---
# Test genre

Do the thing.`;

const execution = { toolCallId: "call-1" };

function loaderWith(...sources: string[]): SkillLoader {
	return new SkillLoader({ includeBundled: false, definitions: sources });
}

/** `execute` is typed to allow a streaming result; the skill tool never streams. */
async function invokeSkill(
	tool: ReturnType<typeof buildSkillTool>,
	name: string,
): Promise<{ name: string; body: string }> {
	return (await tool.execute({ name }, execution)) as {
		name: string;
		body: string;
	};
}

describe("SkillLoader frontmatter", () => {
	it("parses name, description and body", () => {
		const skill = loaderWith(RAW).get("test-genre");
		expect(skill).toEqual({
			name: "test-genre",
			description: "A test genre playbook.",
			body: "# Test genre\n\nDo the thing.",
		});
	});

	it("strips quotes and tolerates CRLF", () => {
		const skill = loaderWith(
			"---\r\nname: \"quoted\"\r\ndescription: 'q'\r\n---\r\nBody.",
		).get("quoted");
		expect(skill?.description).toBe("q");
		expect(skill?.body).toBe("Body.");
	});

	it("rejects a definition with no name", () => {
		expect(() => loaderWith("---\ndescription: x\n---\nBody.")).toThrow(
			/missing a name/,
		);
	});

	it("rejects a definition with an empty body", () => {
		expect(() => loaderWith("---\nname: hollow\n---\n\n")).toThrow(
			/empty body/,
		);
	});

	it("lets a later definition win by name", () => {
		const loader = loaderWith(RAW);
		loader.load({ name: "test-genre", description: "d2", body: "Second." });
		expect(loader.list()).toHaveLength(1);
		expect(loader.get("test-genre")?.body).toBe("Second.");
	});
});

describe("bundled skills", () => {
	it("discovers the bundled SKILL.md files", () => {
		const names = new SkillLoader().names();
		expect(names).toContain("dubstep");
		expect(names).toContain("four-on-the-floor");
		expect(names).toHaveLength(BUNDLED_SKILL_SOURCES.length);
	});

	it("gives every bundled skill a description", () => {
		for (const skill of new SkillLoader().list()) {
			expect(skill.description.length).toBeGreaterThan(0);
			expect(skill.body.length).toBeGreaterThan(0);
		}
	});
});

describe("skill tool", () => {
	it("enumerates every skill as `name: description` in its description", () => {
		const description = skillToolDescription(
			loaderWith(RAW, "---\nname: other\ndescription: Another.\n---\nBody."),
		);
		expect(description).toContain("- test-genre: A test genre playbook.");
		expect(description).toContain("- other: Another.");
	});

	it("says so when there are no skills", () => {
		expect(skillToolDescription(loaderWith())).toContain("- none");
	});

	it("returns the skill body when invoked", async () => {
		const tool = buildSkillTool(loaderWith(RAW));
		const output = await invokeSkill(tool, "test-genre");
		expect(output.body).toBe("# Test genre\n\nDo the thing.");
		expect(
			await tool.toModelOutput?.({
				toolCallId: "call-1",
				input: { name: "test-genre" },
				output,
			}),
		).toEqual({ type: "text", value: "# Test genre\n\nDo the thing." });
	});

	it("errors on an unknown name and lists the valid ones", () => {
		const tool = buildSkillTool(loaderWith(RAW));
		expect(() => tool.execute({ name: "garage" }, execution)).toThrow(
			'Unknown skill "garage". Available skills: test-genre.',
		);
	});

	it("resolves the real bundled skills", async () => {
		const tool = buildSkillTool();
		const output = await invokeSkill(tool, "dubstep");
		expect(output.body).toContain("halftime");
		expect(tool.description).toContain("- dubstep:");
	});
});
