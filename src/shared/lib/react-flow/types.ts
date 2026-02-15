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
	bpmLabel?: string;
};

export type ViewChannelNodeData = BaseNodeData & {
	viewSamples: Signal | null;
};

export type UvViewNodeData = BaseNodeData & {
	viewSamples: Signal | null;
};

export type AudioInputNodeData = BaseNodeData;
export type BeatClockNodeData = BaseNodeData;

export interface MelSpecNodeData extends BaseNodeData {
	melSpec?: {
		width: number;
		height: number;
		data: number[];
		beatGrid: BeatGrid | null;
	};
	isWaiting?: boolean;
}
