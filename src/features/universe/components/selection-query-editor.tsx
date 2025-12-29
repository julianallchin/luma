import {
	useCallback,
	useId,
	useLayoutEffect,
	useMemo,
	useRef,
	useState,
} from "react";
import { cn } from "@/shared/lib/utils";

type SelectionToken = {
	token: string;
	description: string;
	category: "Keyword" | "Type" | "Capability" | "Spatial";
};

type HighlightToken = {
	text: string;
	type:
		| "keyword"
		| "type"
		| "capability"
		| "spatial"
		| "operator"
		| "paren"
		| "text";
};

const TOKEN_OPTIONS: SelectionToken[] = [
	{
		token: "all",
		description: "Select every fixture in the venue.",
		category: "Keyword",
	},
	{
		token: "moving_head",
		description: "Moving head fixtures (pan + tilt).",
		category: "Type",
	},
	{
		token: "moving_spot",
		description: "Alias for moving_head.",
		category: "Type",
	},
	{
		token: "scanner",
		description: "Scanner fixtures (mirror-based movement).",
		category: "Type",
	},
	{
		token: "par_wash",
		description: "Par wash fixtures.",
		category: "Type",
	},
	{
		token: "pixel_bar",
		description: "Pixel bar fixtures.",
		category: "Type",
	},
	{
		token: "strobe",
		description: "Strobe fixtures.",
		category: "Type",
	},
	{
		token: "static",
		description: "Static fixtures (no movement).",
		category: "Type",
	},
	{
		token: "has_color",
		description: "Fixture has color mixing or color wheel.",
		category: "Capability",
	},
	{
		token: "has_movement",
		description: "Fixture has pan/tilt.",
		category: "Capability",
	},
	{
		token: "has_strobe",
		description: "Fixture has shutter/strobe capability.",
		category: "Capability",
	},
	{
		token: "left",
		description: "Group is on the left (axis_lr < 0).",
		category: "Spatial",
	},
	{
		token: "right",
		description: "Group is on the right (axis_lr > 0).",
		category: "Spatial",
	},
	{
		token: "front",
		description: "Group is toward the front (axis_fb < 0).",
		category: "Spatial",
	},
	{
		token: "back",
		description: "Group is toward the back (axis_fb > 0).",
		category: "Spatial",
	},
	{
		token: "high",
		description: "Group is above center (axis_ab > 0).",
		category: "Spatial",
	},
	{
		token: "low",
		description: "Group is below center (axis_ab < 0).",
		category: "Spatial",
	},
	{
		token: "center",
		description: "Group is near the spatial center.",
		category: "Spatial",
	},
	{
		token: "along_major_axis",
		description: "Group aligns with the venue's major axis.",
		category: "Spatial",
	},
	{
		token: "along_minor_axis",
		description: "Group aligns with the venue's minor axis.",
		category: "Spatial",
	},
	{
		token: "is_circular",
		description: "Group's fixtures form a circular layout.",
		category: "Spatial",
	},
];

const TYPE_CAPABILITY_OPTIONS = TOKEN_OPTIONS.filter(
	(option) =>
		option.category === "Type" ||
		option.category === "Capability" ||
		option.category === "Keyword",
);
const SPATIAL_OPTIONS = TOKEN_OPTIONS.filter(
	(option) => option.category === "Spatial",
);

const OPERATORS = [
	{ token: "|", description: "Union" },
	{ token: "&", description: "Intersection" },
	{ token: "^", description: "Exclusive choice (random)" },
	{ token: "~", description: "Negate" },
	{ token: ">", description: "Fallback (use right if left empty)" },
];

const TOKEN_REGEX = /[a-zA-Z0-9_]/;

const KEYWORD_SET = new Set(["all"]);
const TYPE_SET = new Set([
	"moving_head",
	"moving_spot",
	"scanner",
	"par_wash",
	"pixel_bar",
	"strobe",
	"static",
]);
const CAPABILITY_SET = new Set(["has_color", "has_movement", "has_strobe"]);
const SPATIAL_SET = new Set([
	"left",
	"right",
	"front",
	"back",
	"high",
	"low",
	"center",
	"along_major_axis",
	"along_minor_axis",
	"is_circular",
]);
const OPERATOR_SET = new Set(["|", "&", "^", "~", ">"]);
const PAREN_SET = new Set(["(", ")"]);

function tokenize(text: string): HighlightToken[] {
	const tokens: HighlightToken[] = [];
	let i = 0;

	while (i < text.length) {
		const char = text[i];

		// Whitespace
		if (/\s/.test(char)) {
			let ws = char;
			i++;
			while (i < text.length && /\s/.test(text[i])) {
				ws += text[i];
				i++;
			}
			tokens.push({ text: ws, type: "text" });
			continue;
		}

		// Operators
		if (OPERATOR_SET.has(char)) {
			tokens.push({ text: char, type: "operator" });
			i++;
			continue;
		}

		// Parentheses
		if (PAREN_SET.has(char)) {
			tokens.push({ text: char, type: "paren" });
			i++;
			continue;
		}

		// Word tokens
		if (TOKEN_REGEX.test(char)) {
			let word = char;
			i++;
			while (i < text.length && TOKEN_REGEX.test(text[i])) {
				word += text[i];
				i++;
			}
			const lower = word.toLowerCase();
			if (KEYWORD_SET.has(lower)) {
				tokens.push({ text: word, type: "keyword" });
			} else if (TYPE_SET.has(lower)) {
				tokens.push({ text: word, type: "type" });
			} else if (CAPABILITY_SET.has(lower)) {
				tokens.push({ text: word, type: "capability" });
			} else if (SPATIAL_SET.has(lower)) {
				tokens.push({ text: word, type: "spatial" });
			} else {
				tokens.push({ text: word, type: "text" });
			}
			continue;
		}

		// Any other character
		tokens.push({ text: char, type: "text" });
		i++;
	}

	return tokens;
}

const TOKEN_COLORS: Record<HighlightToken["type"], string> = {
	keyword: "text-purple-400",
	type: "text-blue-400",
	capability: "text-green-400",
	spatial: "text-amber-400",
	operator: "text-rose-400",
	paren: "text-gray-400",
	text: "text-foreground",
};

type SelectionRange = {
	start: number;
	end: number;
};

function getSelectionRange(element: HTMLElement): SelectionRange {
	const selection = window.getSelection();
	if (!selection || selection.rangeCount === 0) return { start: 0, end: 0 };

	const range = selection.getRangeAt(0);

	const preStartRange = range.cloneRange();
	preStartRange.selectNodeContents(element);
	preStartRange.setEnd(range.startContainer, range.startOffset);
	const start = preStartRange.toString().length;

	const preEndRange = range.cloneRange();
	preEndRange.selectNodeContents(element);
	preEndRange.setEnd(range.endContainer, range.endOffset);
	const end = preEndRange.toString().length;

	return { start, end };
}

function setSelectionRange(
	element: HTMLElement,
	start: number,
	end: number,
): void {
	const selection = window.getSelection();
	if (!selection) return;

	const findPosition = (
		targetOffset: number,
	): { node: Node; offset: number } | null => {
		let currentOffset = 0;
		const walker = document.createTreeWalker(element, NodeFilter.SHOW_TEXT);
		let node = walker.nextNode();

		while (node) {
			const nodeLength = node.textContent?.length ?? 0;
			if (currentOffset + nodeLength >= targetOffset) {
				return { node, offset: targetOffset - currentOffset };
			}
			currentOffset += nodeLength;
			node = walker.nextNode();
		}
		return null;
	};

	const startPos = findPosition(start);
	const endPos = findPosition(end);

	if (startPos && endPos) {
		const range = document.createRange();
		range.setStart(startPos.node, startPos.offset);
		range.setEnd(endPos.node, endPos.offset);
		selection.removeAllRanges();
		selection.addRange(range);
	} else {
		// Fallback: place cursor at end
		const range = document.createRange();
		range.selectNodeContents(element);
		range.collapse(false);
		selection.removeAllRanges();
		selection.addRange(range);
	}
}

function getPlainText(element: HTMLElement): string {
	return element.textContent ?? "";
}

type CaretPosition = {
	left: number;
	top: number;
	height: number;
};

function getTokenAtCursor(value: string, cursor: number) {
	let start = cursor;
	while (start > 0 && TOKEN_REGEX.test(value[start - 1] ?? "")) {
		start -= 1;
	}
	let end = cursor;
	while (end < value.length && TOKEN_REGEX.test(value[end] ?? "")) {
		end += 1;
	}
	return {
		start,
		end,
		token: value.slice(start, end),
	};
}

function replaceRange(value: string, start: number, end: number, next: string) {
	return `${value.slice(0, start)}${next}${value.slice(end)}`;
}

function getCaretPositionFromContentEditable(
	element: HTMLElement,
): CaretPosition {
	const selection = window.getSelection();
	if (!selection || selection.rangeCount === 0) {
		return { left: 0, top: 0, height: 16 };
	}

	const range = selection.getRangeAt(0);
	const rects = range.getClientRects();
	const elementRect = element.getBoundingClientRect();
	const computedStyle = window.getComputedStyle(element);
	const lineHeight =
		Number.parseFloat(computedStyle.lineHeight) ||
		Number.parseFloat(computedStyle.fontSize) ||
		16;

	if (rects.length > 0) {
		const rect = rects[0];
		return {
			left: rect.left - elementRect.left,
			top: rect.top - elementRect.top,
			height: lineHeight,
		};
	}

	// Fallback for empty or start of line
	return { left: 0, top: 0, height: lineHeight };
}

type SelectionQueryInputProps = {
	label: string;
	value: string;
	onChange: (next: string) => void;
	options: SelectionToken[];
	placeholder: string;
};

function renderHighlightedHTML(text: string): string {
	if (!text) return "";
	const tokens = tokenize(text);
	return tokens
		.map((t) => {
			const escaped = t.text
				.replace(/&/g, "&amp;")
				.replace(/</g, "&lt;")
				.replace(/>/g, "&gt;");
			const colorClass = TOKEN_COLORS[t.type];
			return `<span class="${colorClass}">${escaped}</span>`;
		})
		.join("");
}

function SelectionQueryInput({
	label,
	value,
	onChange,
	options,
	placeholder,
}: SelectionQueryInputProps) {
	const labelId = useId();
	const editorRef = useRef<HTMLDivElement | null>(null);
	const [isFocused, setIsFocused] = useState(false);
	const [activeToken, setActiveToken] = useState("");
	const [tokenRange, setTokenRange] = useState({ start: 0, end: 0 });
	const [selectedIndex, setSelectedIndex] = useState(0);
	const [suggestionsActive, setSuggestionsActive] = useState(false);
	const [hasSelection, setHasSelection] = useState(false);
	const [caretPosition, setCaretPosition] = useState<CaretPosition>({
		left: 0,
		top: 0,
		height: 0,
	});
	const lastValueRef = useRef(value);

	const suggestions = useMemo(() => {
		if (!isFocused || hasSelection || !suggestionsActive) return [];
		const prefix = activeToken.toLowerCase();
		if (prefix.length === 0) return [];
		return options.filter((opt) => opt.token.toLowerCase().startsWith(prefix));
	}, [activeToken, hasSelection, isFocused, options, suggestionsActive]);

	const updateCaret = useCallback(() => {
		const editor = editorRef.current;
		if (!editor) return;
		setCaretPosition(getCaretPositionFromContentEditable(editor));
	}, []);

	const refreshTokenState = useCallback(() => {
		const editor = editorRef.current;
		if (!editor) return;
		const currentValue = getPlainText(editor);
		const selection = window.getSelection();
		if (!selection || selection.rangeCount === 0) return;

		const { start, end } = getSelectionRange(editor);
		const selectionActive = start !== end;
		setHasSelection(selectionActive);

		if (selectionActive) {
			setActiveToken("");
			setTokenRange({ start, end });
			setSuggestionsActive(false);
			setSelectedIndex(0);
			return;
		}

		const tokenInfo = getTokenAtCursor(currentValue, start);
		setActiveToken(tokenInfo.token);
		setTokenRange({ start: tokenInfo.start, end: tokenInfo.end });
		setSelectedIndex(0);
		if (tokenInfo.token.length === 0) {
			setSuggestionsActive(false);
		}
	}, []);

	const applyHighlighting = useCallback((restoreSelection = false) => {
		const editor = editorRef.current;
		if (!editor) return;

		const text = getPlainText(editor);
		const html = renderHighlightedHTML(text);

		if (restoreSelection) {
			const { start, end } = getSelectionRange(editor);
			editor.innerHTML = html;
			setSelectionRange(editor, start, end);
		} else {
			editor.innerHTML = html;
		}
	}, []);

	const applySuggestion = useCallback(
		(token: string) => {
			const editor = editorRef.current;
			if (!editor) return;
			const currentValue = getPlainText(editor);
			const nextValue = replaceRange(
				currentValue,
				tokenRange.start,
				tokenRange.end,
				token,
			);
			const cursorPos = tokenRange.start + token.length;
			lastValueRef.current = nextValue;
			onChange(nextValue);

			// Apply highlighting immediately for autocomplete
			const html = renderHighlightedHTML(nextValue);
			editor.innerHTML = html;
			setSelectionRange(editor, cursorPos, cursorPos);
			editor.focus();

			setSuggestionsActive(false);
			requestAnimationFrame(() => {
				refreshTokenState();
				updateCaret();
			});
		},
		[
			onChange,
			refreshTokenState,
			tokenRange.end,
			tokenRange.start,
			updateCaret,
		],
	);

	// Initialize highlighting on mount and sync external value changes
	useLayoutEffect(() => {
		const editor = editorRef.current;
		if (!editor) return;

		// Check if value changed externally
		if (value !== lastValueRef.current || editor.textContent === "") {
			lastValueRef.current = value;
			const isFocusedNow = document.activeElement === editor;
			const { start, end } = isFocusedNow
				? getSelectionRange(editor)
				: { start: value.length, end: value.length };

			const html = renderHighlightedHTML(value);
			editor.innerHTML = html;

			if (isFocusedNow) {
				setSelectionRange(
					editor,
					Math.min(start, value.length),
					Math.min(end, value.length),
				);
			}
		}
	}, [value]);

	const handleInput = useCallback(() => {
		const editor = editorRef.current;
		if (!editor) return;

		const newValue = getPlainText(editor);
		const { start } = getSelectionRange(editor);
		lastValueRef.current = newValue;
		onChange(newValue);

		// DON'T highlight during editing - it breaks selection
		// Highlighting happens on blur only

		// Determine if we should show suggestions
		if (start > 0) {
			const charBefore = newValue[start - 1] ?? "";
			if (TOKEN_REGEX.test(charBefore)) {
				setSuggestionsActive(true);
			} else {
				setSuggestionsActive(false);
			}
		} else {
			setSuggestionsActive(false);
		}

		requestAnimationFrame(() => {
			refreshTokenState();
			updateCaret();
		});
	}, [onChange, refreshTokenState, updateCaret]);

	return (
		<div className="flex flex-col gap-2">
			<div
				id={labelId}
				className="text-[10px] uppercase tracking-wider text-muted-foreground"
			>
				{label}
			</div>
			<div className="relative">
				{/* biome-ignore lint/a11y/useSemanticElements: ContentEditable is required for syntax highlighting. */}
				<div
					ref={editorRef}
					aria-labelledby={labelId}
					aria-multiline="true"
					contentEditable
					role="textbox"
					suppressContentEditableWarning
					spellCheck={false}
					tabIndex={0}
					onMouseDown={() => {
						setSuggestionsActive(false);
					}}
					onMouseUp={() => {
						refreshTokenState();
						updateCaret();
					}}
					onFocus={() => {
						setIsFocused(true);
						setSuggestionsActive(false);
						refreshTokenState();
						updateCaret();
					}}
					onBlur={() => {
						setIsFocused(false);
						setSuggestionsActive(false);
						setHasSelection(false);
						// Apply highlighting on blur (don't restore selection since we're leaving)
						applyHighlighting(false);
					}}
					onInput={handleInput}
					onClick={() => {
						setSuggestionsActive(false);
						refreshTokenState();
						updateCaret();
					}}
					onSelect={() => {
						refreshTokenState();
						updateCaret();
					}}
					onKeyUp={(event) => {
						if (
							event.key === "Escape" ||
							event.key === "Enter" ||
							event.key === "ArrowUp" ||
							event.key === "ArrowDown"
						) {
							return;
						}
						refreshTokenState();
						updateCaret();
					}}
					onKeyDown={(event) => {
						if (event.key.length === 1 && !TOKEN_REGEX.test(event.key)) {
							setSuggestionsActive(false);
						}
						if (event.key === "Enter") {
							event.preventDefault();
							if (suggestions.length > 0) {
								const option = suggestions[selectedIndex];
								if (option) applySuggestion(option.token);
							}
							setSuggestionsActive(false);
							return;
						}
						if (event.key === "Escape") {
							event.preventDefault();
							setSuggestionsActive(false);
							return;
						}
						if (suggestions.length === 0) return;
						if (event.key === "ArrowDown") {
							event.preventDefault();
							setSelectedIndex((prev) => (prev + 1) % suggestions.length);
							return;
						}
						if (event.key === "ArrowUp") {
							event.preventDefault();
							setSelectedIndex((prev) =>
								prev === 0 ? suggestions.length - 1 : prev - 1,
							);
							return;
						}
						if (event.key === "Tab") {
							event.preventDefault();
							const option = suggestions[selectedIndex];
							if (option) applySuggestion(option.token);
							return;
						}
					}}
					className="w-full min-h-[36px] rounded-md border border-border bg-background px-3 py-2 font-mono text-xs leading-relaxed shadow-sm focus:outline-none focus:ring-2 focus:ring-ring whitespace-pre-wrap break-words selection:bg-blue-500/30"
				/>

				{/* Placeholder - shown when empty */}
				{!value && (
					<div className="absolute left-0 top-0 px-3 py-2 text-xs font-mono text-muted-foreground pointer-events-none leading-relaxed">
						{placeholder}
					</div>
				)}

				{isFocused && suggestions.length > 0 && (
					<div
						className="absolute z-20 flex items-start"
						style={{
							left: Math.max(0, caretPosition.left),
							top: caretPosition.top + caretPosition.height + 6,
						}}
					>
						<div className="max-h-56 w-44 overflow-auto rounded-md border border-border bg-popover shadow-lg">
							{suggestions.map((option, index) => (
								<button
									key={option.token}
									type="button"
									className={cn(
										"flex w-full items-center gap-2 px-3 py-2 text-left text-xs transition-colors",
										index === selectedIndex
											? "bg-accent text-accent-foreground"
											: "text-foreground hover:bg-muted",
									)}
									onMouseEnter={() => setSelectedIndex(index)}
									onMouseDown={(event) => {
										event.preventDefault();
										applySuggestion(option.token);
									}}
								>
									<span className="font-mono">{option.token}</span>
								</button>
							))}
						</div>
						<div className="w-56 rounded-md border border-border -ml-1 bg-popover px-3 py-2 text-xs shadow-lg">
							<div className="text-[10px] uppercase tracking-wider text-muted-foreground">
								{suggestions[selectedIndex]?.category ?? "Token"}
							</div>
							<div className="mt-1 font-mono text-foreground">
								{suggestions[selectedIndex]?.token ?? ""}
							</div>
							<div className="mt-1 text-[11px] text-muted-foreground">
								{suggestions[selectedIndex]?.description ?? ""}
							</div>
						</div>
					</div>
				)}
			</div>
		</div>
	);
}

interface SelectionQueryEditorProps {
	typeValue: string;
	spatialValue: string;
	onChangeType: (next: string) => void;
	onChangeSpatial: (next: string) => void;
}

export function SelectionQueryEditor({
	typeValue,
	spatialValue,
	onChangeType,
	onChangeSpatial,
}: SelectionQueryEditorProps) {
	return (
		<div className="flex flex-col gap-4">
			<SelectionQueryInput
				label="Type + Capability"
				value={typeValue}
				onChange={onChangeType}
				options={TYPE_CAPABILITY_OPTIONS}
				placeholder="e.g. moving_head | strobe & has_movement"
			/>
			<SelectionQueryInput
				label="Spatial"
				value={spatialValue}
				onChange={onChangeSpatial}
				options={SPATIAL_OPTIONS}
				placeholder="e.g. left | right"
			/>
			<div className="flex flex-wrap items-center gap-2 text-[10px] text-muted-foreground">
				<span className="uppercase tracking-wider">Operators</span>
				{OPERATORS.map((op) => (
					<span
						key={op.token}
						className="rounded border border-border px-1.5 py-0.5 font-mono"
						title={op.description}
					>
						{op.token}
					</span>
				))}
			</div>
		</div>
	);
}
