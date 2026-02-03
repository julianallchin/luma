import { invoke } from "@tauri-apps/api/core";
import { Settings2 } from "lucide-react";
import * as React from "react";
import type { NodeProps } from "reactflow";
import type { PatchedFixture } from "@/bindings/fixtures";
import { useAppViewStore } from "@/features/app/stores/use-app-view-store";
import { useGraphStore } from "@/features/patterns/stores/use-graph-store";
import { TagExpressionEditor } from "@/features/universe/components/tag-expression-editor";
import { useFixtureStore } from "@/features/universe/stores/use-fixture-store";
import { Button } from "@/shared/components/ui/button";
import {
	Popover,
	PopoverContent,
	PopoverTrigger,
} from "@/shared/components/ui/popover";
import { ToggleGroup } from "@/shared/components/ui/toggle-group";
import { BaseNode } from "./base-node";
import type { BaseNodeData } from "./types";

export const SelectNode = React.memo(function SelectNode(
	props: NodeProps<BaseNodeData>,
) {
	const { data, id } = props;
	const params = useGraphStore(
		(state) => state.nodeParams[id] ?? ({} as Record<string, unknown>),
	);
	const setParam = useGraphStore((state) => state.setParam);
	const selectionPreviewSeed = useGraphStore(
		(state) => state.selectionPreviewSeed,
	);
	const currentVenueId = useAppViewStore(
		(state) => state.currentVenue?.id ?? null,
	);
	const setPreviewFixtureIds = useFixtureStore(
		(state) => state.setPreviewFixtureIds,
	);
	const clearPreviewFixtureIds = useFixtureStore(
		(state) => state.clearPreviewFixtureIds,
	);
	const previewRequestRef = React.useRef(0);

	const tagExpression = (params.tag_expression as string) ?? "all";
	const spatialReference = (params.spatial_reference as string) ?? "global";

	const previewSelectionQuery = React.useCallback(
		async (query: string) => {
			if (!currentVenueId) {
				clearPreviewFixtureIds();
				return;
			}
			const normalizedQuery = query.trim().length === 0 ? "all" : query;

			const requestId = ++previewRequestRef.current;
			try {
				const fixtures = await invoke<PatchedFixture[]>(
					"preview_selection_query",
					{
						venueId: currentVenueId,
						query: normalizedQuery,
						seed: selectionPreviewSeed ?? undefined,
					},
				);
				if (requestId === previewRequestRef.current) {
					setPreviewFixtureIds(fixtures.map((fixture) => fixture.id));
				}
			} catch (error) {
				console.error("Failed to preview selection query:", error);
				if (requestId === previewRequestRef.current) {
					clearPreviewFixtureIds();
				}
			}
		},
		[
			currentVenueId,
			setPreviewFixtureIds,
			clearPreviewFixtureIds,
			selectionPreviewSeed,
		],
	);

	// Preview on expression change
	React.useEffect(() => {
		if (selectionPreviewSeed === null) return;
		void previewSelectionQuery(tagExpression);
	}, [tagExpression, selectionPreviewSeed, previewSelectionQuery]);

	const paramControls = (
		<div className="p-1">
			<Popover>
				<PopoverTrigger asChild>
					<Button
						variant="outline"
						size="sm"
						className="w-full justify-start gap-2 h-8 text-xs font-normal"
					>
						<Settings2 className="h-3 w-3" />
						{tagExpression || "Configure"}
					</Button>
				</PopoverTrigger>
				<PopoverContent
					className="w-80"
					side="bottom"
					align="center"
					sideOffset={8}
				>
					<div className="space-y-4">
						<div>
							<div className="text-[10px] uppercase tracking-wider text-muted-foreground mb-2">
								Tag Expression
							</div>
							<TagExpressionEditor
								value={tagExpression}
								onChange={(next) => {
									setParam(id, "tag_expression", next);
									void previewSelectionQuery(next);
								}}
								venueId={currentVenueId}
							/>
						</div>

						<div>
							<div className="text-[10px] uppercase tracking-wider text-muted-foreground mb-2">
								Positions
							</div>
							<ToggleGroup
								value={spatialReference}
								onChange={(val) => setParam(id, "spatial_reference", val)}
								options={[
									{ value: "global", label: "Global" },
									{ value: "group_local", label: "Group-Relative" },
								]}
							/>
						</div>
					</div>
				</PopoverContent>
			</Popover>
		</div>
	);

	return <BaseNode {...props} data={{ ...data, paramControls }} />;
});
