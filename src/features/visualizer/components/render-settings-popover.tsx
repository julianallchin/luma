import { RotateCcw, Settings2, Video } from "lucide-react";
import { Checkbox } from "@/shared/components/ui/checkbox";
import {
	Popover,
	PopoverContent,
	PopoverTrigger,
} from "@/shared/components/ui/popover";
import { Slider } from "@/shared/components/ui/slider";
import { useCameraStore } from "../stores/use-camera-store";
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
						<span className="font-medium">High quality render</span>
						<Checkbox
							checked={store.darkStage}
							onCheckedChange={(v) => store.set({ darkStage: !!v })}
						/>
					</div>
					{store.darkStage && (
						<>
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
									onChange={(e) =>
										store.set({ hazeSteps: Number(e.target.value) })
									}
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
						</>
					)}
					<div className="h-px bg-neutral-800" />
					<div>
						<span className="text-neutral-400">Render scale</span>
						<Slider
							min={50}
							max={100}
							value={Math.round((store.maxDpr ?? 2) * 50)}
							onChange={(e) =>
								store.set({ maxDpr: Number(e.target.value) / 50 })
							}
							className="mt-1"
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

const DEFAULT_CAMERA_POSITION: [number, number, number] = [0, 1, 3];
const DEFAULT_CAMERA_TARGET: [number, number, number] = [0, 0, 0];
const DEFAULT_FOV = 50;

export function CameraControlsTrigger({ className }: { className?: string }) {
	const fov = useRenderSettingsStore((s) => s.fov ?? DEFAULT_FOV);
	const setRenderSettings = useRenderSettingsStore((s) => s.set);
	const setCamera = useCameraStore((s) => s.setCamera);

	return (
		<Popover>
			<PopoverTrigger asChild>
				<button
					type="button"
					className={
						className ??
						"text-muted-foreground hover:text-foreground p-1 rounded"
					}
					title="Camera controls"
				>
					<Video className="size-3.5" />
				</button>
			</PopoverTrigger>
			<PopoverContent className="w-56 text-[11px] font-mono bg-neutral-950 border-neutral-800 text-neutral-200 p-3">
				<div className="space-y-2.5">
					<div>
						<span className="text-neutral-400">Field of view</span>
						<Slider
							min={20}
							max={120}
							value={fov}
							onChange={(e) =>
								setRenderSettings({ fov: Number(e.target.value) })
							}
							className="mt-1"
						/>
					</div>
					<div className="h-px bg-neutral-800" />
					<button
						type="button"
						className="flex items-center gap-1.5 text-neutral-400 hover:text-neutral-200 transition-colors"
						onClick={() => {
							setCamera(DEFAULT_CAMERA_POSITION, DEFAULT_CAMERA_TARGET);
							setRenderSettings({ fov: DEFAULT_FOV });
						}}
					>
						<RotateCcw className="size-3" />
						<span>Reset camera</span>
					</button>
				</div>
			</PopoverContent>
		</Popover>
	);
}
