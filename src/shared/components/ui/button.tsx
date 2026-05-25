import { Slot } from "@radix-ui/react-slot";
import type * as React from "react";

import { cn } from "@/shared/lib/utils";

// One button style for the whole app: tight, square, uppercase bold,
// dark fill on darker border. Callers compose with `className` for things
// like layout (`w-full`) — variants are intentionally not a thing.
export const BUTTON_CLASS = cn(
	"inline-flex items-center justify-center gap-1 whitespace-nowrap shrink-0",
	"h-6 px-2 rounded-none border",
	"text-[9px] uppercase tracking-wider font-bold",
	"bg-control border-control-border text-foreground/90",
	"hover:bg-hover hover:text-foreground",
	"transition-colors",
	"disabled:pointer-events-none disabled:opacity-50",
	"[&_svg]:pointer-events-none [&_svg]:shrink-0",
	"[&_svg:not([class*='size-'])]:size-3",
	"outline-none focus:outline-none focus-visible:outline-none focus-visible:ring-0",
);

function Button({
	className,
	asChild = false,
	...props
}: React.ComponentProps<"button"> & { asChild?: boolean }) {
	const Comp = asChild ? Slot : "button";

	return (
		<Comp
			data-slot="button"
			className={cn(BUTTON_CLASS, className)}
			{...props}
		/>
	);
}

export { Button };
