import type { ReactNode } from "react";

import { Button } from "@/shared/components/ui/button";
import { Checkbox } from "@/shared/components/ui/checkbox";
import { Input } from "@/shared/components/ui/input";
import {
	Select,
	SelectContent,
	SelectItem,
	SelectTrigger,
	SelectValue,
} from "@/shared/components/ui/select";
import { Selector } from "@/shared/components/ui/selector";

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
];
