import type {
  BeatGrid,
  NodeTypeDef,
  PortType,
  PatternEntrySummary,
  Series,
} from "../../bindings/schema";

export type PortDirection = "in" | "out";

export interface PortDef {
  id: string;
  label: string;
  direction: PortDirection;
  portType: PortType;
}

export interface BaseNodeData {
  title: string;
  inputs: PortDef[];
  outputs: PortDef[];
  typeId: string;
  definition: NodeTypeDef;
  params: Record<string, unknown>;
  viewSamples?: number[] | null;
  onChange: () => void;
}

export interface ViewChannelNodeData extends BaseNodeData {
  viewSamples: number[] | null;
  seriesData?: Series | null;
  playbackSourceId?: string | null;
}

export interface MelSpecNodeData extends BaseNodeData {
  melSpec?: {
    width: number;
    height: number;
    data: number[];
    beatGrid: BeatGrid | null;
  };
  isWaiting?: boolean;
  playbackSourceId?: string | null;
}

export interface PatternEntryNodeData extends BaseNodeData {
  patternEntry?: PatternEntrySummary | null;
}
