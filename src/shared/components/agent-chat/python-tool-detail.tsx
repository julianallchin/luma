import { code as codePlugin } from "@streamdown/code";
import { Streamdown } from "streamdown";
import type { PythonToolOutput } from "@/bindings/schema";
import type { ToolView } from "./parts";

const CODE_CLASSNAME =
	"agent-tool-code text-xs [&>*:first-child]:mt-0 [&>*:last-child]:mb-0";

function pythonCode(input: unknown): string {
	const code = (input as { code?: unknown } | undefined)?.code;
	return typeof code === "string" ? code.trim() : "";
}

function outputText(output: Partial<PythonToolOutput>): string {
	const sections: string[] = [];
	for (const notice of output.notices ?? []) sections.push(notice);
	if (output.stdout?.trim()) sections.push(output.stdout.trimEnd());
	if (output.stderr?.trim()) sections.push(output.stderr.trimEnd());
	if (output.traceback?.trim()) sections.push(output.traceback.trimEnd());
	if (output.repr?.trim()) sections.push(output.repr.trimEnd());
	return sections.join("\n\n") || "(no output)";
}

/** Compact notebook-style Python detail: highlighted source, one divider, then
 * only the cell's meaningful output. */
export function renderPythonToolDetail(tool: ToolView): React.ReactNode {
	const code = pythonCode(tool.input);
	const output = (tool.output ?? {}) as Partial<PythonToolOutput>;
	const figures = output.figures ?? [];
	return (
		<div className="min-w-0">
			{code ? (
				<Streamdown
					mode="static"
					controls={false}
					plugins={{ code: codePlugin }}
					lineNumbers
					className={CODE_CLASSNAME}
				>{`\`\`\`python\n${code}\n\`\`\``}</Streamdown>
			) : null}
			{tool.output !== undefined || tool.error ? (
				<div className={code ? "mt-2 border-t border-border/70 pt-2" : ""}>
					<pre
						className={
							"max-h-28 overflow-auto whitespace-pre-wrap break-words font-mono text-[11px] leading-relaxed " +
							(tool.error ? "text-destructive" : "text-foreground/75")
						}
					>
						{tool.error ?? outputText(output)}
					</pre>
					{figures.map((figure, index) =>
						figure.base64Png ? (
							<img
								key={`${tool.callId}-figure-${index}`}
								src={`data:image/png;base64,${figure.base64Png}`}
								alt="Python output"
								className="mt-2 max-w-full rounded-sm"
							/>
						) : null,
					)}
				</div>
			) : null}
		</div>
	);
}
