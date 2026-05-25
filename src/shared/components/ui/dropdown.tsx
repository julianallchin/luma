import { ChevronDown } from "lucide-react";
import type { ReactNode } from "react";
import { Button } from "./button";
import {
	DropdownMenu,
	DropdownMenuContent,
	DropdownMenuItem,
	DropdownMenuTrigger,
} from "./dropdown-menu";

export type DropdownItem = {
	label: ReactNode;
	/** Free-text key for React; falls back to stringified label if omitted. */
	key?: string;
	onClick?: () => void;
	disabled?: boolean;
};

type DropdownProps = {
	/** Visible label on the trigger (left-aligned, chevron on the right). */
	label: ReactNode;
	items: DropdownItem[];
	align?: "start" | "end" | "center";
	disabled?: boolean;
	/** Optional className passthrough on the trigger Button. */
	className?: string;
};

/// One pattern for every dropdown: the trigger renders an invisible stack of
/// all item labels inside a 1×1 grid, so the trigger button is exactly as
/// wide as the widest item — pure CSS, no JS measurement. Content then
/// pins to `--radix-dropdown-menu-trigger-width` so trigger and menu read as
/// one continuous surface.
export function Dropdown({
	label,
	items,
	align = "end",
	disabled,
	className,
}: DropdownProps) {
	return (
		<DropdownMenu>
			<DropdownMenuTrigger asChild>
				<Button
					disabled={disabled}
					className={`grid grid-cols-1 grid-rows-1 ${className ?? ""}`}
				>
					<div
						aria-hidden
						className="col-start-1 row-start-1 invisible flex flex-col pointer-events-none"
					>
						{/* Every ghost row mirrors the visible trigger layout
						    (label + gap-2 + chevron). The cell sizes to the widest
						    row, so trigger width = max over (label, ...items) +
						    gap-2 + chevron. Invariant across `label` changes — a
						    Selector swapping its label between options stays the
						    same width. */}
						<span className="flex items-center justify-between gap-2 whitespace-nowrap">
							{label}
							<ChevronDown />
						</span>
						{items.map((it) => (
							<span
								key={it.key ?? String(it.label)}
								className="flex items-center justify-between gap-2 whitespace-nowrap"
							>
								{it.label}
								<ChevronDown />
							</span>
						))}
					</div>
					<span className="col-start-1 row-start-1 flex items-center justify-between gap-2">
						{label}
						<ChevronDown />
					</span>
				</Button>
			</DropdownMenuTrigger>
			<DropdownMenuContent
				align={align}
				className="w-[var(--radix-dropdown-menu-trigger-width)]"
			>
				{items.map((it) => (
					<DropdownMenuItem
						key={it.key ?? String(it.label)}
						disabled={it.disabled}
						onClick={it.onClick}
					>
						{it.label}
					</DropdownMenuItem>
				))}
			</DropdownMenuContent>
		</DropdownMenu>
	);
}
