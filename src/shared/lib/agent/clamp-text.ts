/** Head+tail clamp for model-facing tool text.
 *
 * The transcript/UI keeps full captures; this trims what enters the model's
 * context. Middle-out: the head usually carries the shape of an output and the
 * tail carries the conclusion (or the error), so both survive and an explicit
 * marker tells the model output was dropped — and to inspect smaller slices
 * rather than print everything.
 */
export function clampForModel(
	text: string,
	maxChars: number,
	{
		label = "output",
		tailShare = 0.4,
	}: { label?: string; tailShare?: number } = {},
): string {
	if (text.length <= maxChars) return text;
	const tailChars = Math.floor(maxChars * tailShare);
	const headChars = maxChars - tailChars;
	const omitted = text.length - maxChars;
	const marker = `\n… [${omitted} chars of ${label} omitted — inspect smaller slices instead of printing everything]\n`;
	return (
		text.slice(0, headChars) + marker + text.slice(text.length - tailChars)
	);
}
