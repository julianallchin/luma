import { invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useMemo, useState } from "react";
import type { FixtureTag } from "@/bindings/tags";
import { cn } from "@/shared/lib/utils";

type TagToken = {
	token: string;
	description: string;
	category: "Spatial" | "Purpose" | "Meta" | "Capability";
};

type HighlightToken = {
	text: string;
	type: "tag" | "capability" | "operator" | "paren" | "text";
};

const CAPABILITY_TOKENS: TagToken[] = [
	{
		token: "has_color",
		description: "Fixture has color mixing or color wheel",
		category: "Capability",
	},
	{
		token: "has_movement",
		description: "Fixture has pan/tilt",
		category: "Capability",
	},
	{
		token: "has_strobe",
		description: "Fixture has shutter/strobe capability",
		category: "Capability",
	},
];

const OPERATORS = [
	{ token: "|", description: "Union (OR)" },
	{ token: "&", description: "Intersection (AND)" },
	{ token: "^", description: "Exclusive choice (random)" },
	{ token: "~", description: "Negate (NOT)" },
	{ token: ">", description: "Fallback (use right if left empty)" },
];

const TOKEN_REGEX = /[a-zA-Z0-9_]/;
const OPERATOR_SET = new Set(["|", "&", "^", "~", ">"]);
const PAREN_SET = new Set(["(", ")"]);
const CAPABILITY_SET = new Set(["has_color", "has_movement", "has_strobe"]);

const TOKEN_COLORS: Record<HighlightToken["type"], string> = {
	tag: "text-amber-400",
	capability: "text-green-400",
	operator: "text-rose-400",
	paren: "text-gray-400",
	text: "text-foreground",
};

function tokenize(text: string, tagNames: Set<string>): HighlightToken[] {
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
			if (CAPABILITY_SET.has(lower)) {
				tokens.push({ text: word, type: "capability" });
			} else if (tagNames.has(lower) || lower === "all") {
				tokens.push({ text: word, type: "tag" });
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

interface TagExpressionEditorProps {
	value: string;
	onChange: (value: string) => void;
	venueId: number | null;
}

export function TagExpressionEditor({
	value,
	onChange,
	venueId,
}: TagExpressionEditorProps) {
	const [tags, setTags] = useState<FixtureTag[]>([]);
	const [isFocused, setIsFocused] = useState(false);
	const [cursorPosition, setCursorPosition] = useState(0);
	const [selectedIndex, setSelectedIndex] = useState(0);

	// Load tags for venue
	useEffect(() => {
		if (!venueId) {
			setTags([]);
			return;
		}
		invoke<FixtureTag[]>("list_tags_for_venue", { venueId })
			.then(setTags)
			.catch((e) => console.error("Failed to load tags:", e));
	}, [venueId]);

	const tagNames = useMemo(
		() => new Set(tags.map((t) => t.name.toLowerCase())),
		[tags],
	);

	const allTokenOptions = useMemo((): TagToken[] => {
		const tagTokens: TagToken[] = tags.map((t) => ({
			token: t.name,
			description: `${t.category} tag`,
			category:
				t.category === "spatial"
					? "Spatial"
					: t.category === "purpose"
						? "Purpose"
						: "Meta",
		}));
		return [
			{ token: "all", description: "Select all fixtures", category: "Meta" },
			...tagTokens,
			...CAPABILITY_TOKENS,
		];
	}, [tags]);

	// Get current word being typed
	const getCurrentWord = useCallback((text: string, cursor: number) => {
		let start = cursor;
		while (start > 0 && TOKEN_REGEX.test(text[start - 1])) {
			start--;
		}
		let end = cursor;
		while (end < text.length && TOKEN_REGEX.test(text[end])) {
			end++;
		}
		return { start, end, word: text.slice(start, end) };
	}, []);

	const suggestions = useMemo(() => {
		if (!isFocused) return [];
		const { word } = getCurrentWord(value, cursorPosition);
		const prefix = word.toLowerCase();
		if (!prefix) return allTokenOptions.slice(0, 10);
		return allTokenOptions
			.filter((t) => t.token.toLowerCase().startsWith(prefix))
			.slice(0, 10);
	}, [isFocused, value, cursorPosition, getCurrentWord, allTokenOptions]);

	const applySuggestion = useCallback(
		(token: string) => {
			const { start, end } = getCurrentWord(value, cursorPosition);
			const newValue = value.slice(0, start) + token + value.slice(end);
			onChange(newValue);
		},
		[value, cursorPosition, getCurrentWord, onChange],
	);

	const handleKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
		if (suggestions.length > 0) {
			if (e.key === "ArrowDown") {
				e.preventDefault();
				setSelectedIndex((i) => (i + 1) % suggestions.length);
				return;
			}
			if (e.key === "ArrowUp") {
				e.preventDefault();
				setSelectedIndex(
					(i) => (i - 1 + suggestions.length) % suggestions.length,
				);
				return;
			}
			if (e.key === "Tab" || e.key === "Enter") {
				e.preventDefault();
				const selected = suggestions[selectedIndex];
				if (selected) applySuggestion(selected.token);
				return;
			}
		}
		if (e.key === "Escape") {
			(e.target as HTMLInputElement).blur();
		}
	};

	// Render highlighted text
	const highlightedTokens = tokenize(value, tagNames);

	return (
		<div className="relative">
			<div className="relative">
				{/* Hidden input for actual editing */}
				<input
					type="text"
					value={value}
					onChange={(e) => {
						onChange(e.target.value);
						setCursorPosition(e.target.selectionStart ?? 0);
					}}
					onFocus={() => setIsFocused(true)}
					onBlur={() => setTimeout(() => setIsFocused(false), 150)}
					onSelect={(e) =>
						setCursorPosition(
							(e.target as HTMLInputElement).selectionStart ?? 0,
						)
					}
					onKeyDown={handleKeyDown}
					className="w-full rounded-md border border-border bg-background px-3 py-2 font-mono text-xs text-transparent caret-foreground focus:outline-none focus:ring-2 focus:ring-ring"
					placeholder="e.g. left & blinder"
				/>
				{/* Highlighted overlay */}
				<div className="absolute inset-0 px-3 py-2 font-mono text-xs pointer-events-none overflow-hidden whitespace-pre">
					{highlightedTokens.map((t, i) => (
						// biome-ignore lint/suspicious/noArrayIndexKey: tokens are positional and static
						<span key={i} className={TOKEN_COLORS[t.type]}>
							{t.text}
						</span>
					))}
				</div>
			</div>

			{/* Suggestions dropdown */}
			{isFocused && suggestions.length > 0 && (
				<div className="absolute z-20 mt-1 w-full max-h-48 overflow-auto rounded-md border border-border bg-popover shadow-lg">
					{suggestions.map((opt, i) => (
						<button
							key={opt.token}
							type="button"
							className={cn(
								"flex w-full items-center justify-between px-3 py-2 text-left text-xs",
								i === selectedIndex
									? "bg-accent text-accent-foreground"
									: "hover:bg-muted",
							)}
							onMouseEnter={() => setSelectedIndex(i)}
							onMouseDown={(e) => {
								e.preventDefault();
								applySuggestion(opt.token);
							}}
						>
							<span className="font-mono">{opt.token}</span>
							<span className="text-muted-foreground text-[10px]">
								{opt.category}
							</span>
						</button>
					))}
				</div>
			)}

			{/* Operators help */}
			<div className="flex flex-wrap items-center gap-2 mt-2 text-[10px] text-muted-foreground">
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
