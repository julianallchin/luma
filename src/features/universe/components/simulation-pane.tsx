import { StageVisualizer } from "../../visualizer/components/stage-visualizer";

export function SimulationPane({ readOnly = false }: { readOnly?: boolean }) {
	return <StageVisualizer enableEditing={!readOnly} forceLightStage />;
}
