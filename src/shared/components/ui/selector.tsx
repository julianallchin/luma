import type { ReactNode } from "react";
import { Dropdown } from "./dropdown";

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
};

/// Selection control: pick one of N options. Visually identical to
/// `<Dropdown>` — same trigger shape, same auto-sized width, same item
/// styling — but the trigger label reflects the currently selected value
/// instead of a fixed action label.
export function Selector({
	value,
	onChange,
	options,
	placeholder,
	align,
}: SelectorProps) {
	const selected = options.find((o) => o.value === value);
	const label = selected?.label ?? placeholder ?? "Select…";
	return (
		<Dropdown
			label={label}
			align={align}
			items={options.map((opt) => ({
				label: opt.label,
				key: opt.value,
				disabled: opt.disabled,
				onClick: () => onChange(opt.value),
			}))}
		/>
	);
}
