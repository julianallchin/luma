import { z } from "zod";
import { tool } from "@/shared/lib/agent/agent-tool";
import { parseFrontmatter } from "@/shared/lib/agent/frontmatter";
import { BUNDLED_SKILL_SOURCES } from "./bundled-skills";

/** A genre/technique playbook the agent can pull into context on demand. */
export type Skill = {
	/** Stable identifier the model passes to the `skill` tool. */
	name: string;
	/** One line in the tool description; it is the only thing the model sees
	 * before choosing to load the skill, so it must say when to reach for it. */
	description: string;
	/** Full markdown instructions, returned verbatim as the tool result. */
	body: string;
};

export type SkillDefinition = string | Skill;

function fromRaw(content: string): Skill {
	const { data, body } = parseFrontmatter(content);
	const name = data.name?.trim();
	if (!name) throw new Error("Skill definition is missing a name.");
	if (!body.trim()) throw new Error(`Skill "${name}" has an empty body.`);
	return { name, description: data.description?.trim() ?? "", body };
}

function normalize(definition: SkillDefinition): Skill {
	const skill =
		typeof definition === "string" ? fromRaw(definition) : definition;
	const name = skill.name.trim();
	if (!name) throw new Error("Skill definition is missing a name.");
	return {
		name,
		description: skill.description.trim(),
		body: skill.body.trim(),
	};
}

/** Registry of bundled (and, in tests, injected) skills. */
export class SkillLoader {
	private readonly skills = new Map<string, Skill>();

	constructor(
		options: {
			definitions?: SkillDefinition[];
			includeBundled?: boolean;
		} = {},
	) {
		if (options.includeBundled !== false) {
			for (const source of BUNDLED_SKILL_SOURCES) this.load(source);
		}
		for (const definition of options.definitions ?? []) this.load(definition);
	}

	/** Add or replace a definition. Later definitions win by name. */
	load(definition: SkillDefinition): Skill {
		const skill = normalize(definition);
		this.skills.set(skill.name, skill);
		return skill;
	}

	get(name: string): Skill | undefined {
		return this.skills.get(name.trim());
	}

	list(): Skill[] {
		return [...this.skills.values()];
	}

	names(): string[] {
		return [...this.skills.keys()];
	}
}

export function skillToolDescription(loader: SkillLoader): string {
	const skills = loader
		.list()
		.map((skill) => `- ${skill.name}: ${skill.description}`)
		.join("\n");
	return `Load a scoring playbook — genre-specific technique written by lighting designers who score this music by hand. Returns the full instructions as the tool result.

Available skills:
${skills || "- none"}

Read the skill that matches the track before you plan a show for it: it tells you what the feature set does and does not measure for that genre, which analysis to derive yourself in Python, and how much of the work to delegate. Load it once per thread and keep working from it.`;
}

/** The `skill` tool. Its description enumerates the bundled skills; invoking it
 * with a name returns that skill's markdown body verbatim. */
export function buildSkillTool(loader: SkillLoader = new SkillLoader()) {
	return tool({
		get description() {
			return skillToolDescription(loader);
		},
		inputSchema: z.object({
			name: z
				.string()
				.min(1)
				.describe("Skill name from the available skills above."),
		}),
		execute: ({ name }): { name: string; body: string } => {
			const skill = loader.get(name);
			if (!skill) {
				throw new Error(
					`Unknown skill "${name}". Available skills: ${loader.names().join(", ") || "none"}.`,
				);
			}
			return { name: skill.name, body: skill.body };
		},
		toModelOutput: ({ output }) => ({
			type: "text" as const,
			value: output.body,
		}),
	});
}
