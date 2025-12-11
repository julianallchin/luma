import * as React from "react";
import { cn } from "@/shared/lib/utils";

const Slider = React.forwardRef<
	HTMLInputElement,
	React.InputHTMLAttributes<HTMLInputElement>
>(({ className, min = 0, max = 100, onChange, ...props }, ref) => {
	// Handle value for visual representation (supports both controlled and uncontrolled modes)
	const [internalValue, setInternalValue] = React.useState(
		props.defaultValue ?? min,
	);
	const value = props.value ?? internalValue;

	// Calculate percentage for the yellow fill bar
	const safeMin = Number(min);
	const safeMax = Number(max);
	const currentVal = Number(value);
	const percentage = ((currentVal - safeMin) / (safeMax - safeMin)) * 100;

	// Intercept onChange to update internal state for uncontrolled usage
	const handleChange = (e: React.ChangeEvent<HTMLInputElement>) => {
		if (props.value === undefined) {
			setInternalValue(Number(e.target.value));
		}
		onChange?.(e);
	};

	return (
		<div
			className={cn(
				"relative h-7 w-full bg-input border border-border group focus-within:border-primary transition-colors select-none overflow-hidden",
				className,
			)}
		>
			{/* Visual Fill Bar (Ableton Style) */}
			<div
				className="absolute top-0 left-0 h-full bg-primary opacity-20 pointer-events-none transition-all duration-75 ease-out"
				style={{ width: `${Math.min(100, Math.max(0, percentage))}%` }}
			/>

			{/* Numeric Value Overlay */}
			<div className="absolute inset-0 flex items-center px-2 pointer-events-none z-10">
				<span className="text-primary text-[10px] font-mono truncate">
					{value}
				</span>
			</div>

			{/* The Actual Input (Invisible but interactive) */}
			<input
				type="range"
				className="absolute inset-0 w-full h-full opacity-0 cursor-ew-resize appearance-none m-0 p-0 z-20 focus:outline-none"
				ref={ref}
				min={min}
				max={max}
				value={value}
				onChange={handleChange}
				{...props}
			/>
		</div>
	);
});
Slider.displayName = "Slider";

export { Slider };
