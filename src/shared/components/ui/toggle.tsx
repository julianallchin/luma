import { Slot } from "@radix-ui/react-slot";
import type * as React from "react";

import { cn } from "@/shared/lib/utils";

type ToggleProps = React.ComponentProps<"button"> & {
	pressed: boolean;
	asChild?: boolean;
};

/// Single-element toggle button. Matches `<Button>` geometry but lights up
/// (bg → foreground, text → background) when `pressed`. Use for sticky
/// filter-style toggles that should look like a single pressable slab — the
/// button itself is the on/off control.
function Toggle({
	className,
	pressed,
	asChild = false,
	...props
}: ToggleProps) {
	const Comp = asChild ? Slot : "button";
	return (
		<Comp
			type="button"
			data-slot="toggle"
			aria-pressed={pressed}
			data-state={pressed ? "on" : "off"}
			className={cn(
				"inline-flex items-center justify-center gap-1 whitespace-nowrap shrink-0",
				"h-6 px-2 rounded-none border",
				"text-[9px] uppercase tracking-wider font-bold",
				"transition-colors",
				"disabled:pointer-events-none disabled:opacity-50",
				"[&_svg]:pointer-events-none [&_svg]:shrink-0",
				"[&_svg:not([class*='size-'])]:size-3",
				"outline-none focus:outline-none focus-visible:outline-none focus-visible:ring-0",
				pressed
					? "bg-foreground border-control-border text-background"
					: "bg-control border-control-border text-foreground/90 hover:bg-hover hover:text-foreground",
				className,
			)}
			{...props}
		/>
	);
}

export { Toggle };
