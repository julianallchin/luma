import { Toggle } from "@/shared/components/ui/toggle";
import { cn } from "@/shared/lib/utils";

interface ToggleGroupOption {
	value: string;
	label: string;
}

interface ToggleGroupProps {
	value: string;
	options: ToggleGroupOption[];
	onChange: (value: string) => void;
	className?: string;
}

/// Horizontal segmented control. Each segment is a `<Toggle>` so the
/// pressed visual is consistent with standalone toggles elsewhere;
/// segments share borders by collapsing the negative margin gap.
function ToggleGroup({
	value,
	options,
	onChange,
	className,
}: ToggleGroupProps) {
	return (
		<div className={cn("flex", className)}>
			{options.map((opt, i) => (
				<Toggle
					key={opt.value}
					pressed={value === opt.value}
					onClick={() => onChange(opt.value)}
					className={cn(i > 0 && "-ml-px", value === opt.value && "z-10")}
				>
					{opt.label}
				</Toggle>
			))}
		</div>
	);
}

export { ToggleGroup, type ToggleGroupOption, type ToggleGroupProps };
