import type {
	BeatGrid,
	NodeTypeDef,
	PortType,
	Signal,
} from "@/bindings/schema";

export type PortDef = {
	id: string;
	label: string;
	direction: "in" | "out";
	portType: PortType;
};

// Canonical per-port-type colors — shared by the edge renderer and the
// node handles so a wire and the port it plugs into read as the same type.
export const PORT_TYPE_COLORS: Record<PortType, string> = {
	Intensity: "#f59e0b", // amber-500
	Audio: "#3b82f6", // blue-500
	BeatGrid: "#10b981", // emerald-500
	Series: "#8b5cf6", // violet-500 (Legacy/Viewers)
	Color: "#ec4899", // pink-500
	Signal: "#22d3ee", // cyan-400
	Selection: "#c084fc", // purple-400
	Events: "#ef4444", // red-500
	Stops: "#f472b6", // pink-400 — palette/gradient Stops
};

export const DEFAULT_PORT_COLOR = "#6b7280"; // gray-500

export type BaseNodeData = {
	title: string;
	inputs: PortDef[];
	outputs: PortDef[];
	typeId: string;
	definition: NodeTypeDef;
	params: Record<string, unknown>;
	onChange: () => void;
	paramControls?: React.ReactNode;
	trackName?: string;
	timeLabel?: string;
};

export type ViewChannelNodeData = BaseNodeData & {
	viewSamples: Signal | null;
};

export type UvViewNodeData = BaseNodeData & {
	viewSamples: Signal | null;
};

export type AudioInputNodeData = BaseNodeData;

export interface MelSpecNodeData extends BaseNodeData {
	melSpec?: {
		width: number;
		height: number;
		data: number[];
		beatGrid: BeatGrid | null;
	};
	isWaiting?: boolean;
}
