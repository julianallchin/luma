import * as SelectPrimitive from "@radix-ui/react-select";
import { CheckIcon, ChevronDownIcon } from "lucide-react";
import type * as React from "react";

import { cn } from "@/shared/lib/utils";

function Select({
	...props
}: React.ComponentProps<typeof SelectPrimitive.Root>) {
	return <SelectPrimitive.Root data-slot="select" {...props} />;
}

function SelectGroup({
	...props
}: React.ComponentProps<typeof SelectPrimitive.Group>) {
	return <SelectPrimitive.Group data-slot="select-group" {...props} />;
}

function SelectValue({
	...props
}: React.ComponentProps<typeof SelectPrimitive.Value>) {
	return <SelectPrimitive.Value data-slot="select-value" {...props} />;
}

function SelectTrigger({
	className,
	children,
	sizingOptions,
	...props
}: React.ComponentProps<typeof SelectPrimitive.Trigger> & {
	/**
	 * When provided, the trigger reserves width for the widest option via an
	 * invisible ghost stack (pure CSS, no JS measurement) — so a row of selects
	 * stays aligned and the trigger does not resize when the value changes.
	 * `<Selector>` passes this; raw `<Select>` users size with `className`
	 * (`w-full`, `w-28`, …) instead.
	 */
	sizingOptions?: { key: string; label: React.ReactNode }[];
}) {
	const icon = (
		<SelectPrimitive.Icon asChild>
			<ChevronDownIcon className="size-3 opacity-50" />
		</SelectPrimitive.Icon>
	);
	return (
		<SelectPrimitive.Trigger
			data-slot="select-trigger"
			className={cn(
				"flex h-6 w-fit items-center justify-between gap-2 whitespace-nowrap border px-2 text-xs",
				"rounded-none bg-control border-control-border text-foreground/90",
				"transition-colors hover:bg-hover hover:text-foreground",
				"outline-none focus:outline-none focus-visible:outline-none focus-visible:ring-0",
				"disabled:cursor-not-allowed disabled:opacity-50",
				"data-[placeholder]:text-muted-foreground",
				"*:data-[slot=select-value]:line-clamp-1 *:data-[slot=select-value]:flex *:data-[slot=select-value]:items-center *:data-[slot=select-value]:gap-2",
				"[&_svg]:pointer-events-none [&_svg]:shrink-0 [&_svg:not([class*='size-'])]:size-3",
				sizingOptions && "grid grid-cols-1 grid-rows-1",
				className,
			)}
			{...props}
		>
			{sizingOptions ? (
				<>
					{/* Ghost stack: every option rendered exactly like the visible
					    row, invisible, so the grid cell — and thus the trigger — is
					    as wide as the widest option. Invariant across value changes. */}
					<span
						aria-hidden
						className="col-start-1 row-start-1 invisible pointer-events-none flex flex-col"
					>
						{sizingOptions.map((opt) => (
							<span
								key={opt.key}
								className="flex items-center justify-between gap-2 whitespace-nowrap"
							>
								{opt.label}
								<ChevronDownIcon className="size-3" />
							</span>
						))}
					</span>
					<span className="col-start-1 row-start-1 flex items-center justify-between gap-2">
						{children}
						{icon}
					</span>
				</>
			) : (
				<>
					{children}
					{icon}
				</>
			)}
		</SelectPrimitive.Trigger>
	);
}

function SelectContent({
	className,
	children,
	position = "popper",
	align = "center",
	sideOffset = -1,
	// Radix Select defaults collisionPadding to 10px; near a window edge that
	// nudges the menu sideways off the trigger. 0 keeps it flush under the
	// trigger (matches DropdownMenu, which uses the Popper default of 0).
	collisionPadding = 0,
	...props
}: React.ComponentProps<typeof SelectPrimitive.Content>) {
	return (
		<SelectPrimitive.Portal>
			<SelectPrimitive.Content
				data-slot="select-content"
				className={cn(
					"relative z-50 max-h-(--radix-select-content-available-height) overflow-x-hidden overflow-y-auto",
					"rounded-none border p-0 shadow-none",
					"bg-control border-control-border text-foreground/90",
					// Size to the menu's own content at a fixed 1x size, like a
					// native select popup. Do NOT pin to --radix-select-trigger-width:
					// that var is in screen px, so inside the zoomable canvas the menu
					// box scaled with zoom and the fixed-size font looked like it was
					// resizing. The trigger's ghost-stack sizing is unaffected.
					className,
				)}
				position={position}
				align={align}
				sideOffset={sideOffset}
				collisionPadding={collisionPadding}
				{...props}
			>
				<SelectPrimitive.Viewport className="p-0">
					{children}
				</SelectPrimitive.Viewport>
			</SelectPrimitive.Content>
		</SelectPrimitive.Portal>
	);
}

function SelectLabel({
	className,
	...props
}: React.ComponentProps<typeof SelectPrimitive.Label>) {
	return (
		<SelectPrimitive.Label
			data-slot="select-label"
			className={cn("text-muted-foreground px-2 py-1.5 text-xs", className)}
			{...props}
		/>
	);
}

function SelectItem({
	className,
	children,
	...props
}: React.ComponentProps<typeof SelectPrimitive.Item>) {
	return (
		<SelectPrimitive.Item
			data-slot="select-item"
			className={cn(
				"relative flex h-[22px] w-full cursor-default items-center gap-2 pr-8 pl-2 text-xs outline-hidden select-none",
				// Match the tracklist row: instant highlight on enter, 150ms
				// fade-out on leave (see track-browser.tsx).
				"text-foreground/90 transition-colors duration-150 hover:duration-0 data-[highlighted]:duration-0 hover:bg-hover hover:text-foreground data-[highlighted]:bg-hover data-[highlighted]:text-foreground",
				"data-[disabled]:pointer-events-none data-[disabled]:opacity-50",
				className,
			)}
			{...props}
		>
			<span className="absolute right-2 flex size-3.5 items-center justify-center">
				<SelectPrimitive.ItemIndicator>
					<CheckIcon className="size-3" />
				</SelectPrimitive.ItemIndicator>
			</span>
			<SelectPrimitive.ItemText>{children}</SelectPrimitive.ItemText>
		</SelectPrimitive.Item>
	);
}

function SelectSeparator({
	className,
	...props
}: React.ComponentProps<typeof SelectPrimitive.Separator>) {
	return (
		<SelectPrimitive.Separator
			data-slot="select-separator"
			className={cn("bg-border pointer-events-none -mx-1 my-1 h-px", className)}
			{...props}
		/>
	);
}

export {
	Select,
	SelectContent,
	SelectGroup,
	SelectItem,
	SelectLabel,
	SelectSeparator,
	SelectTrigger,
	SelectValue,
};
