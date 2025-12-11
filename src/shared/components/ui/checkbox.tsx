import * as CheckboxPrimitive from "@radix-ui/react-checkbox";
import type * as React from "react";

import { cn } from "@/shared/lib/utils";

function Checkbox({
	className,
	...props
}: React.ComponentProps<typeof CheckboxPrimitive.Root>) {
	return (
		<CheckboxPrimitive.Root
			data-slot="checkbox"
			className={cn(
				"peer border-border dark:bg-input data-[state=checked]:border-primary data-[state=checked]:border-4 focus-visible:border-ring focus-visible:ring-ring/50 aria-invalid:ring-destructive/20 dark:aria-invalid:ring-destructive/40 aria-invalid:border-destructive size-3 shrink-0 border shadow-xs transition-shadow outline-none focus-visible:ring-[3px] disabled:cursor-not-allowed disabled:opacity-50",
				className,
			)}
			{...props}
		></CheckboxPrimitive.Root>
	);
}

export { Checkbox };
