import { identicon } from "@dicebear/collection";
import { createAvatar } from "@dicebear/core";
import { useMemo } from "react";
import { cn } from "@/shared/lib/utils";

/** Stable generated identity shared by chips, the list, and drill-in header. */
export function SubagentAvatar({
	seed,
	className,
}: {
	seed: string;
	className?: string;
}) {
	const uri = useMemo(
		() => createAvatar(identicon, { seed, scale: 70 }).toDataUri(),
		[seed],
	);
	return (
		<img
			src={uri}
			alt=""
			aria-hidden
			className={cn("size-5 shrink-0 rounded-full", className)}
		/>
	);
}
