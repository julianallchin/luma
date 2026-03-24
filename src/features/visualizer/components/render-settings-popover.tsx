import { Settings2 } from "lucide-react";
import { Checkbox } from "@/shared/components/ui/checkbox";
import {
	Popover,
	PopoverContent,
	PopoverTrigger,
} from "@/shared/components/ui/popover";
import { Slider } from "@/shared/components/ui/slider";
import { useRenderSettingsStore } from "../stores/use-render-settings-store";

export function RenderSettingsTrigger({ className }: { className?: string }) {
	const store = useRenderSettingsStore();
	return (
		<Popover>
			<PopoverTrigger asChild>
				<button
					type="button"
					className={
						className ??
						"text-muted-foreground hover:text-foreground p-1 rounded"
					}
					title="Render settings"
				>
					<Settings2 className="size-3.5" />
				</button>
			</PopoverTrigger>
			<PopoverContent className="w-56 text-[11px] font-mono bg-neutral-950 border-neutral-800 text-neutral-200 p-3">
				<div className="space-y-2.5">
					<div className="flex items-center justify-between">
						<span className="font-medium">Dark stage</span>
						<Checkbox
							checked={store.darkStage}
							onCheckedChange={(v) => store.set({ darkStage: !!v })}
						/>
					</div>
					<div className="h-px bg-neutral-800" />
					<div className="flex items-center justify-between">
						<span>Volumetric haze</span>
						<Checkbox
							checked={store.volumetricHaze}
							onCheckedChange={(v) => store.set({ volumetricHaze: !!v })}
						/>
					</div>
					<div>
						<span className="text-neutral-400">Haze steps</span>
						<Slider
							min={2}
							max={24}
							step={2}
							value={store.hazeSteps}
							onChange={(e) => store.set({ hazeSteps: Number(e.target.value) })}
							className="mt-1"
						/>
					</div>
					<div>
						<span className="text-neutral-400">Haze density</span>
						<Slider
							min={0}
							max={100}
							value={Math.round(store.hazeDensity * 100)}
							onChange={(e) =>
								store.set({ hazeDensity: Number(e.target.value) / 100 })
							}
							className="mt-1"
						/>
					</div>
					<div className="h-px bg-neutral-800" />
					<div className="flex items-center justify-between">
						<span>Scene spotlights</span>
						<Checkbox
							checked={store.fixtureSpotlights}
							onCheckedChange={(v) => store.set({ fixtureSpotlights: !!v })}
						/>
					</div>
					<div>
						<span className="text-neutral-400">Light count</span>
						<Slider
							min={1}
							max={8}
							value={store.spotlightCount}
							onChange={(e) =>
								store.set({ spotlightCount: Number(e.target.value) })
							}
							className="mt-1"
						/>
					</div>
					<div className="flex items-center justify-between">
						<span>Shadows</span>
						<Checkbox
							checked={store.shadows}
							onCheckedChange={(v) => store.set({ shadows: !!v })}
						/>
					</div>
					<div className="h-px bg-neutral-800" />
					<div className="flex items-center justify-between">
						<span>Bloom</span>
						<Checkbox
							checked={store.bloom}
							onCheckedChange={(v) => store.set({ bloom: !!v })}
						/>
					</div>
				</div>
			</PopoverContent>
		</Popover>
	);
}
