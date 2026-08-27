import { z } from "zod";
import { tool } from "@/shared/lib/agent/agent-tool";
import { invoke } from "@/shared/lib/tauri";
// The one copy of the tool description, shared with the Rust agent loop, which
// reads the same file through `include_str!`.
import DESCRIPTION from "../../../../../src-tauri/src/agent/prompts/skill-tool.md?raw";

/** The `<available_skills>` block to append to a system prompt.
 *
 * The webview has no filesystem, so the registry is Rust's — this is the same
 * listing the Rust loop puts in its own prompt, fetched rather than rebuilt.
 * Empty when the bundle carries no readable skill. */
export function skillsListing(): Promise<string> {
	return invoke<string>("skills_listing");
}

/** The `skill` tool: load one playbook by the name the listing gave.
 *
 * The listing lives in the system prompt, not here, so this description is
 * short and static — a tool description that enumerated the skills would move
 * every time a playbook was edited, and it is part of the cached prefix. */
export function buildSkillTool() {
	return tool({
		description: DESCRIPTION.trimEnd(),
		inputSchema: z.object({
			name: z.string().min(1).describe("Skill name from <available_skills>."),
		}),
		execute: async ({ name }): Promise<{ name: string; body: string }> => ({
			name,
			body: await invoke<string>("get_skill", { name }),
		}),
		toModelOutput: ({ output }) => ({
			type: "text" as const,
			value: output.body,
		}),
	});
}
