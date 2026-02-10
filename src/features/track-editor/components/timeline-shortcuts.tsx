import { Keyboard } from "lucide-react";
import {
	Popover,
	PopoverContent,
	PopoverTrigger,
} from "@/shared/components/ui/popover";

type Shortcut = { keys: string; desc: string } | { sep: string };

const shortcuts: Shortcut[] = [
	{ keys: "Click", desc: "Select annotation" },
	{ keys: "Shift+Click", desc: "Add to selection" },
	{ keys: "Double-click", desc: "Edit pattern" },
	{ keys: "Drag header", desc: "Move annotation" },
	{ keys: "Drag edge", desc: "Resize annotation" },
	{ sep: "selection" },
	{ keys: "\u2318Z", desc: "Undo" },
	{ keys: "\u21e7\u2318Z", desc: "Redo" },
	{ sep: "undo" },
	{ keys: "\u2318E", desc: "Split at cursor" },
	{ keys: "\u2325\u2191 / \u2325\u2193", desc: "Move to lane above/below" },
	{ keys: "Del", desc: "Delete selected / region" },
	{ sep: "edit" },
	{ keys: "\u2318C", desc: "Copy" },
	{ keys: "\u2318X", desc: "Cut" },
	{ keys: "\u2318V", desc: "Paste" },
	{ keys: "\u2318D", desc: "Duplicate" },
	{ sep: "clipboard" },
	{ keys: "H", desc: "Auto-fit vertical zoom" },
	{ keys: "Scroll", desc: "Horizontal scroll" },
	{ keys: "\u2318Scroll", desc: "Zoom" },
	{ keys: "\u2325Scroll", desc: "Vertical zoom" },
];

export function TimelineShortcuts() {
	return (
		<Popover>
			<PopoverTrigger asChild>
				<button
					type="button"
					className="absolute bottom-2 right-16 px-2 py-0.5 bg-neutral-900/90 text-[10px] text-neutral-400 font-mono backdrop-blur-sm border border-neutral-800 shadow-sm hover:border-neutral-700 hover:text-neutral-200 transition-colors"
				>
					<Keyboard size={12} />
				</button>
			</PopoverTrigger>
			<PopoverContent
				className="w-72 text-[11px] font-mono bg-neutral-950 border-neutral-800 text-neutral-200"
				align="end"
			>
				<div className="space-y-0.5">
					<div className="text-[10px] text-neutral-500 uppercase tracking-wider mb-1.5">
						Keyboard Shortcuts
					</div>
					{shortcuts.map((s) =>
						"sep" in s ? (
							<div key={s.sep} className="h-px bg-neutral-800 my-1.5" />
						) : (
							<div key={s.keys} className="flex justify-between gap-3">
								<span className="text-neutral-400 shrink-0">{s.keys}</span>
								<span className="text-neutral-300 text-right">{s.desc}</span>
							</div>
						),
					)}
				</div>
			</PopoverContent>
		</Popover>
	);
}
