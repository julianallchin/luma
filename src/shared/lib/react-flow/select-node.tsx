import { invoke } from "@tauri-apps/api/core";
import { Settings2 } from "lucide-react";
import * as React from "react";
import type { NodeProps } from "reactflow";
import type { PatchedFixture } from "@/bindings/fixtures";
import { useAppViewStore } from "@/features/app/stores/use-app-view-store";
import { useGraphStore } from "@/features/patterns/stores/use-graph-store";
import { SelectionQueryEditor } from "@/features/universe/components/selection-query-editor";
import { useFixtureStore } from "@/features/universe/stores/use-fixture-store";
import { Button } from "@/shared/components/ui/button";
import {
	Popover,
	PopoverContent,
	PopoverTrigger,
} from "@/shared/components/ui/popover";
import { BaseNode } from "./base-node";
import type { BaseNodeData } from "./types";

type LegacySelectionQuery = {
	typeFilter?: {
		xor?: string[];
		fallback?: string[];
	};
	spatialFilter?: {
		axis?: string;
		position?: string;
	};
	amount?: {
		mode?: string;
		value?: number;
	};
};

const TYPE_TOKENS = new Set([
	"moving_head",
	"moving_spot",
	"pixel_bar",
	"par_wash",
	"scanner",
	"strobe",
	"static",
	"unknown",
]);

const CAPABILITY_TOKENS = new Set(["has_color", "has_movement", "has_strobe"]);

const SPATIAL_TOKENS = new Set([
	"left",
	"right",
	"front",
	"back",
	"high",
	"low",
	"center",
	"along_major_axis",
	"along_minor_axis",
	"is_circular",
]);

const QUERY_TOKEN_REGEX = /[a-zA-Z0-9_]+/g;

const normalizeTypeToken = (token: string) => {
	const normalized = token.toLowerCase();
	if (normalized === "moving_spot") {
		return "moving_head";
	}
	return TYPE_TOKENS.has(normalized) ? normalized : null;
};

const parseLegacySelectionQuery = (
	raw: unknown,
): LegacySelectionQuery | null => {
	if (!raw) return null;
	if (typeof raw === "string") {
		const trimmed = raw.trim();
		if (!trimmed.startsWith("{")) return null;
		try {
			return JSON.parse(trimmed) as LegacySelectionQuery;
		} catch {
			return null;
		}
	}
	if (typeof raw === "object") {
		return raw as LegacySelectionQuery;
	}
	return null;
};

const wrapIfNeeded = (expr: string) =>
	/[|&^>]/.test(expr) ? `(${expr})` : expr;

const buildSpatialExpression = (axis?: string, position?: string) => {
	if (!axis) return null;
	const normalizedAxis = axis.toLowerCase();
	const normalizedPosition = position?.toLowerCase();
	const mapPos = (positive: string, negative: string) => {
		if (normalizedPosition === "positive") return positive;
		if (normalizedPosition === "negative") return negative;
		if (normalizedPosition === "center") return "center";
		if (normalizedPosition === "both") return `${negative} | ${positive}`;
		return null;
	};

	switch (normalizedAxis) {
		case "lr":
			return mapPos("right", "left");
		case "fb":
			return mapPos("back", "front");
		case "ab":
			return mapPos("high", "low");
		case "major_axis":
			return "along_major_axis";
		case "minor_axis":
			return "along_minor_axis";
		case "any_opposing":
			return "along_major_axis | along_minor_axis";
		default:
			return null;
	}
};

const buildLegacyExpression = (legacy: LegacySelectionQuery) => {
	const xorTokens = (legacy.typeFilter?.xor ?? [])
		.map(normalizeTypeToken)
		.filter(Boolean) as string[];
	const fallbackTokens = (legacy.typeFilter?.fallback ?? [])
		.map(normalizeTypeToken)
		.filter(Boolean) as string[];

	const xorExpr = xorTokens.join(" ^ ");
	const fallbackExpr = fallbackTokens.join(" | ");
	let typeExpr = "";
	if (xorExpr && fallbackExpr) {
		typeExpr = `(${xorExpr}) > (${fallbackExpr})`;
	} else if (xorExpr) {
		typeExpr = xorExpr;
	} else if (fallbackExpr) {
		typeExpr = fallbackExpr;
	}

	const spatialExpr = buildSpatialExpression(
		legacy.spatialFilter?.axis,
		legacy.spatialFilter?.position,
	);

	if (typeExpr && spatialExpr) {
		return `${wrapIfNeeded(typeExpr)} & ${wrapIfNeeded(spatialExpr)}`;
	}
	if (typeExpr) return typeExpr;
	if (spatialExpr) return spatialExpr;
	return "all";
};

const buildCombinedQuery = (typeQuery: string, spatialQuery: string) => {
	const trimmedType = typeQuery.trim();
	const trimmedSpatial = spatialQuery.trim();

	if (trimmedType && trimmedSpatial) {
		return `${wrapIfNeeded(trimmedType)} & ${wrapIfNeeded(trimmedSpatial)}`;
	}
	if (trimmedType) return trimmedType;
	if (trimmedSpatial) return trimmedSpatial;
	return "";
};

const extractQueryTokens = (expr: string): string[] =>
	expr.toLowerCase().match(QUERY_TOKEN_REGEX) ?? [];

const countTokenMatches = (tokens: string[], set: Set<string>) =>
	tokens.reduce((total, token) => total + (set.has(token) ? 1 : 0), 0);

const findTopLevelAndSplits = (expr: string) => {
	const positions: number[] = [];
	let depth = 0;
	for (let i = 0; i < expr.length; i += 1) {
		const char = expr[i];
		if (char === "(") depth += 1;
		if (char === ")") depth = Math.max(0, depth - 1);
		if (char === "&" && depth === 0) {
			positions.push(i);
		}
	}
	return positions;
};

const inferSplitQueries = (expr: string) => {
	const trimmed = expr.trim();
	if (!trimmed) return { type: "", spatial: "" };
	const tokens = extractQueryTokens(trimmed);
	const spatialCount = countTokenMatches(tokens, SPATIAL_TOKENS);
	const typeCount =
		countTokenMatches(tokens, TYPE_TOKENS) +
		countTokenMatches(tokens, CAPABILITY_TOKENS) +
		(tokens.includes("all") ? 1 : 0);

	if (spatialCount > 0 && typeCount === 0) {
		return { type: "", spatial: trimmed };
	}
	if (typeCount > 0 && spatialCount === 0) {
		return { type: trimmed, spatial: "" };
	}

	const positions = findTopLevelAndSplits(trimmed);
	let best: { type: string; spatial: string; score: number } | null = null;

	for (const pos of positions) {
		const left = trimmed.slice(0, pos).trim();
		const right = trimmed.slice(pos + 1).trim();
		if (!left || !right) continue;
		const leftTokens = extractQueryTokens(left);
		const rightTokens = extractQueryTokens(right);
		const leftSpatial = countTokenMatches(leftTokens, SPATIAL_TOKENS);
		const rightSpatial = countTokenMatches(rightTokens, SPATIAL_TOKENS);
		const leftType =
			countTokenMatches(leftTokens, TYPE_TOKENS) +
			countTokenMatches(leftTokens, CAPABILITY_TOKENS);
		const rightType =
			countTokenMatches(rightTokens, TYPE_TOKENS) +
			countTokenMatches(rightTokens, CAPABILITY_TOKENS);
		const score = rightSpatial - rightType + (leftType - leftSpatial);
		if (!best || score > best.score) {
			best = { type: left, spatial: right, score };
		}
	}

	if (best && best.score > 0) {
		return { type: best.type, spatial: best.spatial };
	}

	return { type: trimmed, spatial: "" };
};

export function SelectNode(props: NodeProps<BaseNodeData>) {
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

	const selectionQueryRaw = params.selection_query;
	const legacySelectionQuery = React.useMemo(
		() => parseLegacySelectionQuery(selectionQueryRaw),
		[selectionQueryRaw],
	);
	const legacyExpression = React.useMemo(
		() =>
			legacySelectionQuery ? buildLegacyExpression(legacySelectionQuery) : null,
		[legacySelectionQuery],
	);
	const hasTypeQueryParam = typeof params.selection_query_type === "string";
	const hasSpatialQueryParam =
		typeof params.selection_query_spatial === "string";
	const inferredQueries = React.useMemo(() => {
		if (legacyExpression) {
			return inferSplitQueries(legacyExpression);
		}
		if (typeof selectionQueryRaw === "string") {
			return inferSplitQueries(selectionQueryRaw);
		}
		return { type: "", spatial: "" };
	}, [legacyExpression, selectionQueryRaw]);

	const typeQueryValue = hasTypeQueryParam
		? (params.selection_query_type as string)
		: inferredQueries.type;
	const spatialQueryValue = hasSpatialQueryParam
		? (params.selection_query_spatial as string)
		: inferredQueries.spatial;
	const combinedQuery = buildCombinedQuery(typeQueryValue, spatialQueryValue);
	const selectionQueryValue =
		combinedQuery ||
		(typeof selectionQueryRaw === "string"
			? selectionQueryRaw
			: (legacyExpression ?? ""));

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

	// Migrate legacy JSON format to expression
	React.useEffect(() => {
		if (!legacyExpression) return;
		if (typeof selectionQueryRaw === "string") {
			if (!selectionQueryRaw.trim().startsWith("{")) return;
		} else if (!selectionQueryRaw) {
			return;
		}
		setParam(id, "selection_query", legacyExpression);
	}, [selectionQueryRaw, legacyExpression, setParam, id]);

	// Sync inferred queries to params
	React.useEffect(() => {
		if (!hasTypeQueryParam && inferredQueries.type) {
			setParam(id, "selection_query_type", inferredQueries.type);
		}
		if (!hasSpatialQueryParam && inferredQueries.spatial) {
			setParam(id, "selection_query_spatial", inferredQueries.spatial);
		}
		if (combinedQuery && selectionQueryValue !== combinedQuery) {
			setParam(id, "selection_query", combinedQuery);
		}
	}, [
		hasTypeQueryParam,
		hasSpatialQueryParam,
		inferredQueries.type,
		inferredQueries.spatial,
		combinedQuery,
		selectionQueryValue,
		setParam,
		id,
	]);

	// Preview on query change
	React.useEffect(() => {
		if (selectionPreviewSeed === null) return;
		void previewSelectionQuery(selectionQueryValue);
	}, [selectionQueryValue, selectionPreviewSeed, previewSelectionQuery]);

	const paramControls = (
		<div className="p-1">
			<div className="">
				<Popover>
					<PopoverTrigger asChild>
						<Button
							variant="outline"
							size="sm"
							className="w-full justify-start gap-2 h-8 text-xs font-normal"
						>
							<Settings2 className="h-3 w-3" />
							Configure Query
						</Button>
					</PopoverTrigger>
					<PopoverContent
						className="w-80"
						side="bottom"
						align="center"
						sideOffset={8}
					>
						<div className="space-y-2">
							<div className="text-xs font-medium">Selection Query</div>
							<SelectionQueryEditor
								typeValue={typeQueryValue}
								spatialValue={spatialQueryValue}
								onChangeType={(nextType) => {
									setParam(id, "selection_query_type", nextType);
									const nextQuery = buildCombinedQuery(
										nextType,
										spatialQueryValue,
									);
									setParam(id, "selection_query", nextQuery);
									void previewSelectionQuery(nextQuery);
								}}
								onChangeSpatial={(nextSpatial) => {
									setParam(id, "selection_query_spatial", nextSpatial);
									const nextQuery = buildCombinedQuery(
										typeQueryValue,
										nextSpatial,
									);
									setParam(id, "selection_query", nextQuery);
									void previewSelectionQuery(nextQuery);
								}}
							/>
						</div>
					</PopoverContent>
				</Popover>
			</div>
		</div>
	);

	return <BaseNode {...props} data={{ ...data, paramControls }} />;
}
