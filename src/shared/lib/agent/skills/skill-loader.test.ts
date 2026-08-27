import { afterEach, describe, expect, it, vi } from "vitest";
import { resetInvoke, setInvoke } from "@/shared/lib/tauri";
import { skillToolLabel } from "./index";
import { buildSkillTool, skillsListing } from "./skill-loader";

const execution = { toolCallId: "call-1" };
const ENVELOPE =
	'<skill name="color" location="/skills/color/SKILL.md">\nBody.\n</skill>';

afterEach(resetInvoke);

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

describe("the skill tool", () => {
	it("is a fetch, not a menu — the listing lives in the system prompt", () => {
		const tool = buildSkillTool();
		expect(tool.description).toContain("<available_skills>");
		expect(tool.description).not.toContain("- color:");
	});

	it("returns one skill's envelope from the Rust registry", async () => {
		const dispatch = vi.fn().mockResolvedValue(ENVELOPE);
		setInvoke(dispatch);
		const tool = buildSkillTool();
		const output = await invokeSkill(tool, "color");
		expect(dispatch).toHaveBeenCalledWith("get_skill", { name: "color" });
		expect(output).toEqual({ name: "color", body: ENVELOPE });
		expect(
			await tool.toModelOutput?.({
				toolCallId: "call-1",
				input: { name: "color" },
				output,
			}),
		).toEqual({ type: "text", value: ENVELOPE });
	});

	it("surfaces an unknown name as the registry's own error", async () => {
		setInvoke(() =>
			Promise.reject(new Error("unknown skill 'garage'. Available: color")),
		);
		await expect(invokeSkill(buildSkillTool(), "garage")).rejects.toThrow(
			/unknown skill 'garage'/,
		);
	});

	it("labels the chat row with the requested skill", () => {
		expect(
			skillToolLabel({ name: "skill", input: { name: "heavy-bass" } } as never),
		).toEqual({ verb: "skill", detail: "heavy-bass" });
	});
});

describe("the listing", () => {
	it("is fetched, never rebuilt in the webview", async () => {
		const dispatch = vi
			.fn()
			.mockResolvedValue("<available_skills>\n</available_skills>");
		setInvoke(dispatch);
		expect(await skillsListing()).toContain("<available_skills>");
		expect(dispatch).toHaveBeenCalledWith("skills_listing", undefined);
	});
});
