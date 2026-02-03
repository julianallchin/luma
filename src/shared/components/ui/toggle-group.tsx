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

function ToggleGroup({
	value,
	options,
	onChange,
	className,
}: ToggleGroupProps) {
	return (
		<div className={cn("flex", className)}>
			{options.map((opt, i) => (
				<button
					key={opt.value}
					type="button"
					onClick={() => onChange(opt.value)}
					className={cn(
						"h-7 px-2 text-xs border border-border bg-input transition-colors",
						i === 0 && "rounded-l-md",
						i === options.length - 1 && "rounded-r-md",
						i > 0 && "-ml-px",
						value === opt.value
							? "border-primary bg-primary/10 text-primary z-10"
							: "text-muted-foreground hover:bg-muted hover:text-foreground",
					)}
				>
					{opt.label}
				</button>
			))}
		</div>
	);
}

export { ToggleGroup, type ToggleGroupOption, type ToggleGroupProps };
