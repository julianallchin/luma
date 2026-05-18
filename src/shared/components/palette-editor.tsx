import { Plus, X } from "lucide-react";
import * as React from "react";
import { Button } from "@/shared/components/ui/button";
import { Input } from "@/shared/components/ui/input";
import {
	Popover,
	PopoverContent,
	PopoverTrigger,
} from "@/shared/components/ui/popover";
import {
	ColorPicker,
	ColorPickerAlpha,
	ColorPickerHue,
	ColorPickerSelection,
} from "@/shared/components/ui/shadcn-io/color-picker";

/** A Palette is an ordered set of K colors with uniform spacing. */
export type PaletteValue = {
	colors: string[];
};

/** A Gradient is a 1D color function defined by stops at positions t ∈ [0,1]. */
export type GradientStop = { color: string; t: number };
export type GradientValue = {
	stops: GradientStop[];
};

const FALLBACK_PALETTE: string[] = ["#ff0080", "#00ffc8", "#ffbe28"];
const FALLBACK_GRADIENT: GradientStop[] = [
	{ color: "#000000", t: 0.0 },
	{ color: "#ffffff", t: 1.0 },
];

function rgbaArrayToHex(rgba: unknown): string | null {
	if (!Array.isArray(rgba) || rgba.length < 3) return null;
	const toHex = (v: number) =>
		Math.round(Number(v)).toString(16).padStart(2, "0");
	const r = toHex(rgba[0]);
	const g = toHex(rgba[1]);
	const b = toHex(rgba[2]);
	if (rgba.length >= 4 && rgba[3] !== 1) {
		const a = Math.round(Number(rgba[3]) * 255)
			.toString(16)
			.padStart(2, "0");
		return `#${r}${g}${b}${a}`;
	}
	return `#${r}${g}${b}`;
}

function Swatch({
	color,
	onChange,
}: {
	color: string;
	onChange: (next: string) => void;
}) {
	return (
		<Popover>
			<PopoverTrigger asChild>
				<button
					type="button"
					className="h-7 w-7 rounded border border-border shrink-0"
					style={{ backgroundColor: color }}
					aria-label={`Color ${color}`}
				/>
			</PopoverTrigger>
			<PopoverContent className="w-auto p-3 bg-popover">
				<ColorPicker
					defaultValue={color}
					onChange={(rgba) => {
						const hex = rgbaArrayToHex(rgba);
						if (hex) onChange(hex);
					}}
				>
					<div className="flex flex-col gap-2">
						<ColorPickerSelection className="h-28 w-48 rounded" />
						<ColorPickerHue />
						<ColorPickerAlpha />
					</div>
				</ColorPicker>
			</PopoverContent>
		</Popover>
	);
}

/** Editor for a `Palette` arg / node — K colors, uniform spacing, no t. */
export function PaletteSwatches({
	value,
	onChange,
}: {
	value: PaletteValue;
	onChange: (value: PaletteValue) => void;
}) {
	const colors = value.colors ?? FALLBACK_PALETTE;
	const setColors = (next: string[]) => onChange({ colors: next });

	return (
		<div className="space-y-2 nodrag">
			<div className="flex items-center justify-between gap-2">
				<span className="text-[10px] uppercase tracking-wide text-muted-foreground">
					Colors
				</span>
				<Button
					type="button"
					variant="ghost"
					size="sm"
					className="h-7 px-2"
					onClick={() =>
						setColors([...colors, colors[colors.length - 1] ?? "#ffffff"])
					}
				>
					<Plus className="size-3" />
				</Button>
			</div>
			<div className="space-y-1.5">
				{colors.map((color, i) => (
					// Intentional: changing the color must NOT remount the row,
					// or the open color-picker popover gets torn down mid-drag.
					// biome-ignore lint/suspicious/noArrayIndexKey: see above
					<div key={i} className="flex items-center gap-2">
						<Swatch
							color={color}
							onChange={(next) => {
								const copy = [...colors];
								copy[i] = next;
								setColors(copy);
							}}
						/>
						<code className="text-[10px] text-muted-foreground flex-1 truncate">
							{color}
						</code>
						{colors.length > 1 && (
							<Button
								type="button"
								variant="ghost"
								size="sm"
								className="h-6 w-6 p-0"
								onClick={() => setColors(colors.filter((_, j) => j !== i))}
							>
								<X className="size-3" />
							</Button>
						)}
					</div>
				))}
			</div>
		</div>
	);
}

/** Editor for a `Gradient` arg / node — stops with positions, with preview bar. */
export function GradientStops({
	value,
	onChange,
}: {
	value: GradientValue;
	onChange: (value: GradientValue) => void;
}) {
	const stops = value.stops ?? FALLBACK_GRADIENT;
	const setStops = (next: GradientStop[]) => onChange({ stops: next });

	const gradientPreview = React.useMemo(() => {
		const sorted = [...stops].sort((a, b) => a.t - b.t);
		if (sorted.length === 0) return "var(--muted)";
		if (sorted.length === 1) return sorted[0].color;
		return `linear-gradient(to right, ${sorted
			.map((s) => `${s.color} ${(s.t * 100).toFixed(1)}%`)
			.join(", ")})`;
	}, [stops]);

	return (
		<div className="space-y-2 nodrag">
			<div className="flex items-center justify-between gap-2">
				<span className="text-[10px] uppercase tracking-wide text-muted-foreground">
					Stops
				</span>
				<Button
					type="button"
					variant="ghost"
					size="sm"
					className="h-7 px-2"
					onClick={() => {
						const last = stops[stops.length - 1];
						setStops([
							...stops,
							{
								color: last?.color ?? "#ffffff",
								t: Math.min(1, (last?.t ?? 0) + 0.1),
							},
						]);
					}}
				>
					<Plus className="size-3" />
				</Button>
			</div>
			<div
				className="h-5 w-full rounded border border-border"
				style={{ background: gradientPreview }}
			/>
			<div className="space-y-1.5">
				{stops.map((stop, i) => (
					// Same rationale as above — intentional index key so the
					// open color-picker popover survives stop edits.
					// biome-ignore lint/suspicious/noArrayIndexKey: see above
					<div key={i} className="flex items-center gap-2">
						<Swatch
							color={stop.color}
							onChange={(next) => {
								const copy = [...stops];
								copy[i] = { ...stop, color: next };
								setStops(copy);
							}}
						/>
						<Input
							type="number"
							min={0}
							max={1}
							step={0.01}
							value={stop.t}
							className="h-6 w-16 text-[10px]"
							onChange={(e) => {
								const t = Math.max(0, Math.min(1, Number(e.target.value)));
								const copy = [...stops];
								copy[i] = { ...stop, t };
								setStops(copy);
							}}
						/>
						<code className="text-[10px] text-muted-foreground flex-1 truncate">
							{stop.color}
						</code>
						{stops.length > 1 && (
							<Button
								type="button"
								variant="ghost"
								size="sm"
								className="h-6 w-6 p-0"
								onClick={() => setStops(stops.filter((_, j) => j !== i))}
							>
								<X className="size-3" />
							</Button>
						)}
					</div>
				))}
			</div>
		</div>
	);
}

/** Parse the JSON-encoded `value` param of a Palette or Gradient node. */
export function parsePaletteJson(text: string | undefined): PaletteValue {
	if (!text) return { colors: [...FALLBACK_PALETTE] };
	try {
		const parsed = JSON.parse(text);
		if (Array.isArray(parsed?.colors)) {
			return { colors: parsed.colors };
		}
	} catch {
		// fallthrough
	}
	return { colors: [...FALLBACK_PALETTE] };
}

export function parseGradientJson(text: string | undefined): GradientValue {
	if (!text) return { stops: [...FALLBACK_GRADIENT] };
	try {
		const parsed = JSON.parse(text);
		if (Array.isArray(parsed?.stops)) {
			return { stops: parsed.stops };
		}
	} catch {
		// fallthrough
	}
	return { stops: [...FALLBACK_GRADIENT] };
}
