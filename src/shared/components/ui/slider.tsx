import * as React from "react";

import { cn } from "@/shared/lib/utils";

const Slider = React.forwardRef<
	HTMLInputElement,
	React.InputHTMLAttributes<HTMLInputElement>
>(({ className, ...props }, ref) => {
	return (
		<input
			type="range"
			className={cn(
				"w-full h-2 bg-secondary rounded-lg appearance-none cursor-pointer [&::-webkit-slider-thumb]:appearance-none [&::-webkit-slider-thumb]:w-3 [&::-webkit-slider-thumb]:h-3 [&::-webkit-slider-thumb]:rounded-full [&::-webkit-slider-thumb]:bg-primary [&::-moz-range-thumb]:w-3 [&::-moz-range-thumb]:h-3 [&::-moz-range-thumb]:rounded-full [&::-moz-range-thumb]:bg-primary [&::-moz-range-thumb]:border-0",
				className,
			)}
			ref={ref}
			{...props}
		/>
	);
});
Slider.displayName = "Slider";

export { Slider };
