import type { ToolLabel, ToolView } from "@/shared/components/agent-chat/parts";

export { buildSkillTool, skillsListing } from "./skill-loader";

/** Chat-row label for a skill load: the requested skill name. */
export function skillToolLabel(tool: ToolView): ToolLabel {
	const name = (tool.input as { name?: unknown } | undefined)?.name;
	return {
		verb: "skill",
		detail: typeof name === "string" && name.trim() ? name.trim() : null,
	};
}
