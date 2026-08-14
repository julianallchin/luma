/** Minimal YAML-ish frontmatter: a leading `---` block of `key: value` lines,
 * then the markdown body. Shared by bundled agent definitions and bundled
 * skills, which use the same authoring format. */
export function parseFrontmatter(content: string): {
	data: Record<string, string>;
	body: string;
} {
	const match = content.match(/^---\r?\n([\s\S]*?)\r?\n---\r?\n?([\s\S]*)$/);
	if (!match) return { data: {}, body: content.trim() };
	const data: Record<string, string> = {};
	for (const line of match[1].split("\n")) {
		const separator = line.indexOf(":");
		if (separator === -1) continue;
		const key = line.slice(0, separator).trim();
		const value = line
			.slice(separator + 1)
			.trim()
			.replace(/^["']|["']$/g, "");
		if (key) data[key] = value;
	}
	return { data, body: match[2].trim() };
}
