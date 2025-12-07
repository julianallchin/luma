import type {
	BeatGrid,
	MelSpec,
	NodeTypeDef,
	PatternArgDef,
	PortType,
	Series,
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
	seriesData: Series | null;
};

export interface MelSpecNodeData extends BaseNodeData {
	melSpec?: {
		width: number;
		height: number;
		data: number[];
		beatGrid: BeatGrid | null;
	};
	isWaiting?: boolean;
}

export interface HarmonyColorVisualizerNodeData extends BaseNodeData {
	seriesData?: Series | null; // Color time series with palette indices
	baseColor?: string | null;
}
