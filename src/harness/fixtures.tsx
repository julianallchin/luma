import type { ReactNode } from "react";

import { Button } from "@/shared/components/ui/button";
import { Checkbox } from "@/shared/components/ui/checkbox";
import { Dropdown } from "@/shared/components/ui/dropdown";
import { Input } from "@/shared/components/ui/input";
import {
	InputGroup,
	InputGroupAddon,
	InputGroupButton,
	InputGroupInput,
} from "@/shared/components/ui/input-group";
import { Label } from "@/shared/components/ui/label";
import {
	Select,
	SelectContent,
	SelectItem,
	SelectTrigger,
	SelectValue,
} from "@/shared/components/ui/select";
import { Selector } from "@/shared/components/ui/selector";
import { Slider } from "@/shared/components/ui/slider";
import { Spinner } from "@/shared/components/ui/spinner";
import { Textarea } from "@/shared/components/ui/textarea";
import { Toggle } from "@/shared/components/ui/toggle";
import { ToggleGroup } from "@/shared/components/ui/toggle-group";
import {
	Tooltip,
	TooltipContent,
	TooltipProvider,
	TooltipTrigger,
} from "@/shared/components/ui/tooltip";

// One fixture = one component in one deterministic state, identified by an id
// shared with the GPUI harness (harness/gpui/src/fixtures.rs). Both renderers
// must render the same state for the same id — that is the whole contract.
export interface Fixture {
	id: string;
	render: () => ReactNode;
}

export const FIXTURES: Fixture[] = [
	{
		id: "button",
		render: () => <Button>Import Tracks</Button>,
	},
	{
		id: "button-disabled",
		render: () => <Button disabled>Import Tracks</Button>,
	},
	{
		id: "button-row",
		render: () => (
			<div className="flex gap-2">
				<Button>Save</Button>
				<Button>Cancel</Button>
				<Button>Delete Track</Button>
			</div>
		),
	},
	{
		id: "select",
		render: () => (
			<Select value="opus-5">
				<SelectTrigger className="w-40">
					<SelectValue />
				</SelectTrigger>
				<SelectContent>
					<SelectItem value="opus-5">Opus 5</SelectItem>
					<SelectItem value="kimi-k3-fast">Kimi K3 Fast</SelectItem>
				</SelectContent>
			</Select>
		),
	},
	{
		id: "selector",
		render: () => (
			<Selector
				value="bars"
				onChange={() => {}}
				options={[
					{ value: "bars", label: "Bars" },
					{ value: "beats", label: "Beats" },
					{ value: "seconds", label: "Seconds" },
				]}
			/>
		),
	},
	{
		id: "input",
		render: () => <Input placeholder="Track name" className="w-40" />,
	},
	{
		id: "checkbox-row",
		render: () => (
			<div className="flex items-center gap-2">
				<Checkbox checked />
				<Checkbox />
			</div>
		),
	},
	{
		// Action menu, closed — only the self-sizing trigger is captured. The
		// trigger is as wide as the widest item ("Import From Rekordbox"), which
		// is the geometry a GPUI port has to reproduce.
		id: "dropdown-closed",
		render: () => (
			<Dropdown
				label="Actions"
				items={[
					{ label: "Import From Rekordbox" },
					{ label: "Reanalyze" },
					{ label: "Sign Out" },
				]}
			/>
		),
	},
	{
		id: "label-input-row",
		render: () => (
			<div className="flex items-center gap-2">
				<Label htmlFor="harness-bpm">BPM</Label>
				<Input id="harness-bpm" defaultValue="128" className="w-20" />
			</div>
		),
	},
	{
		id: "input-group",
		render: () => (
			<InputGroup className="w-64">
				<InputGroupInput defaultValue="front_wash" />
				<InputGroupAddon align="inline-end">
					<InputGroupButton>Apply</InputGroupButton>
				</InputGroupAddon>
			</InputGroup>
		),
	},
	{
		id: "textarea",
		render: () => (
			<Textarea
				className="w-64"
				rows={3}
				defaultValue="Strobe the back movers on the drop."
			/>
		),
	},
	{
		// 40 of 0..100 → fill bar covers 40% of the track.
		id: "slider",
		render: () => <Slider className="w-64" min={0} max={100} value={40} />,
	},
	{
		id: "toggle-pressed",
		render: () => <Toggle pressed>Loop</Toggle>,
	},
	{
		id: "toggle-unpressed",
		render: () => <Toggle pressed={false}>Loop</Toggle>,
	},
	{
		id: "toggle-group",
		render: () => (
			<ToggleGroup
				value="beats"
				onChange={() => {}}
				options={[
					{ value: "bars", label: "Bars" },
					{ value: "beats", label: "Beats" },
					{ value: "seconds", label: "Seconds" },
				]}
			/>
		),
	},
	{
		// `animate-none` freezes the spin so the capture is byte-stable; the
		// captured frame is the 0° pose of the same glyph the app spins.
		id: "spinner",
		render: () => <Spinner className="animate-none text-foreground" />,
	},
	{
		// Closed tooltip: only the trigger renders (content is portalled on open,
		// which the harness deliberately doesn't capture yet).
		id: "tooltip-trigger-closed",
		render: () => (
			<TooltipProvider>
				<Tooltip>
					<TooltipTrigger asChild>
						<Button>Reanalyze</Button>
					</TooltipTrigger>
					<TooltipContent>Re-run beat + stem analysis</TooltipContent>
				</Tooltip>
			</TooltipProvider>
		),
	},
];
