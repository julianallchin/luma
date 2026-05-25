import type * as React from "react";

import { cn } from "@/shared/lib/utils";

function Input({ className, type, ...props }: React.ComponentProps<"input">) {
	return (
		<input
			type={type}
			data-slot="input"
			className={cn(
				"h-6 w-full min-w-0 px-2 border rounded-none",
				"bg-control border-control-border text-foreground",
				"text-xs placeholder:text-muted-foreground",
				"selection:bg-primary selection:text-primary-foreground",
				"outline-none focus:outline-none focus-visible:outline-none focus-visible:ring-0",
				"focus-visible:border-ring",
				"aria-invalid:border-destructive",
				"disabled:pointer-events-none disabled:cursor-not-allowed disabled:opacity-50",
				"file:inline-flex file:h-6 file:border-0 file:bg-transparent file:text-xs file:font-medium file:text-foreground",
				className,
			)}
			{...props}
		/>
	);
}

export { Input };
