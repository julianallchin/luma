import type { ReactNode } from "react";
import { cn } from "@/shared/lib/utils";
import {
	Select,
	SelectContent,
	SelectItem,
	SelectTrigger,
	SelectValue,
} from "./select";

export type SelectorOption = {
	value: string;
	label: ReactNode;
	disabled?: boolean;
};

type SelectorProps = {
	value: string | null;
	onChange: (value: string) => void;
	options: SelectorOption[];
	/** Trigger label when no option matches `value`. */
	placeholder?: ReactNode;
	align?: "start" | "end" | "center";
	/**
	 * Hide the currently-selected option from the dropdown list (switcher
	 * behavior — you don't pick the page you're already on). The trigger still
	 * shows it, and the ghost stack still sizes to it.
	 */
	hideSelected?: boolean;
};

/// Selection control: pick one of N options. A thin shorthand over `<Select>`
/// that takes an `options` array instead of compound children, and feeds those
/// options to the trigger's ghost stack so the trigger is sized to the widest
/// option and stays stable across selection changes (keeps a row of selectors
/// aligned). Reach for raw `<Select>` when you want an explicit width instead.
// Brutalist control font — matches <Button> / <Dropdown> so a Selector reads
// the same as its neighbours on a control surface (the raw <Select> primitive
// stays text-xs for inline data pickers).
const SELECTOR_FONT = "text-[9px] uppercase tracking-wider font-bold";

export function Selector({
	value,
	onChange,
	options,
	placeholder,
	align = "end",
	hideSelected,
}: SelectorProps) {
	return (
		<Select value={value ?? undefined} onValueChange={onChange}>
			<SelectTrigger
				className={SELECTOR_FONT}
				sizingOptions={options.map((o) => ({ key: o.value, label: o.label }))}
			>
				<SelectValue placeholder={placeholder ?? "Select…"} />
			</SelectTrigger>
			<SelectContent align={align}>
				{options.map((opt) => (
					// The selected item stays mounted (so the trigger can read its
					// label) but is hidden from the list when hideSelected is set.
					<SelectItem
						key={opt.value}
						value={opt.value}
						disabled={opt.disabled}
						className={cn(
							SELECTOR_FONT,
							hideSelected && opt.value === value && "hidden",
						)}
					>
						{opt.label}
					</SelectItem>
				))}
			</SelectContent>
		</Select>
	);
}
