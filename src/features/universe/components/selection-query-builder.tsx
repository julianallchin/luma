import { Plus, X } from "lucide-react";
import { useCallback, useRef, useState } from "react";
import type {
	Axis,
	AxisPosition,
	FixtureType,
	SelectionQuery,
} from "@/bindings/groups";
import { Input } from "@/shared/components/ui/input";
import { Label } from "@/shared/components/ui/label";
import {
	Select,
	SelectContent,
	SelectItem,
	SelectTrigger,
	SelectValue,
} from "@/shared/components/ui/select";

interface SelectionQueryBuilderProps {
	value: SelectionQuery;
	onChange: (query: SelectionQuery) => void;
}

const FIXTURE_TYPES: { value: FixtureType; label: string }[] = [
	{ value: "moving_head", label: "Moving Head" },
	{ value: "pixel_bar", label: "Pixel Bar" },
	{ value: "par_wash", label: "Par Wash" },
	{ value: "scanner", label: "Scanner" },
	{ value: "strobe", label: "Strobe" },
	{ value: "static", label: "Static" },
];

const AXES: { value: Axis; label: string }[] = [
	{ value: "lr", label: "Left/Right" },
	{ value: "fb", label: "Front/Back" },
	{ value: "ab", label: "Above/Below" },
	{ value: "major_axis", label: "Major Axis" },
	{ value: "minor_axis", label: "Minor Axis" },
	{ value: "any_opposing", label: "Any Opposing" },
];

const AXIS_POSITIONS: { value: AxisPosition; label: string }[] = [
	{ value: "positive", label: "Positive" },
	{ value: "negative", label: "Negative" },
	{ value: "center", label: "Center" },
	{ value: "both", label: "Both" },
];

export function SelectionQueryBuilder({
	value,
	onChange,
}: SelectionQueryBuilderProps) {
	// Use ref to store onChange to avoid stale closures
	const onChangeRef = useRef(onChange);
	onChangeRef.current = onChange;

	// Local state for type filter
	const [xorTypes, setXorTypes] = useState<FixtureType[]>(
		() => value.typeFilter?.xor ?? [],
	);
	const [fallbackTypes, setFallbackTypes] = useState<FixtureType[]>(
		() => value.typeFilter?.fallback ?? [],
	);

	// Local state for spatial filter
	const [spatialEnabled, setSpatialEnabled] = useState(
		() => !!value.spatialFilter,
	);
	const [axis, setAxis] = useState<Axis>(
		() => value.spatialFilter?.axis ?? "lr",
	);
	const [position, setPosition] = useState<AxisPosition>(
		() => value.spatialFilter?.position ?? "both",
	);

	// Local state for amount filter
	const [amountMode, setAmountMode] = useState<
		"all" | "percent" | "count" | "every_other"
	>(() =>
		value.amount ? ("mode" in value.amount ? value.amount.mode : "all") : "all",
	);
	const [amountValue, setAmountValue] = useState<number>(() =>
		value.amount && "value" in value.amount ? value.amount.value : 100,
	);

	// Build query and notify parent
	const notifyChange = useCallback(
		(updates: {
			xorTypes?: FixtureType[];
			fallbackTypes?: FixtureType[];
			spatialEnabled?: boolean;
			axis?: Axis;
			position?: AxisPosition;
			amountMode?: "all" | "percent" | "count" | "every_other";
			amountValue?: number;
		}) => {
			const currentXor = updates.xorTypes ?? xorTypes;
			const currentFallback = updates.fallbackTypes ?? fallbackTypes;
			const currentSpatialEnabled = updates.spatialEnabled ?? spatialEnabled;
			const currentAxis = updates.axis ?? axis;
			const currentPosition = updates.position ?? position;
			const currentAmountMode = updates.amountMode ?? amountMode;
			const currentAmountValue = updates.amountValue ?? amountValue;

			const query: SelectionQuery = {
				typeFilter: null,
				spatialFilter: null,
				amount: null,
			};

			// Type filter
			if (currentXor.length > 0 || currentFallback.length > 0) {
				query.typeFilter = {
					xor: currentXor,
					fallback: currentFallback,
				};
			}

			// Spatial filter
			if (currentSpatialEnabled) {
				query.spatialFilter = {
					axis: currentAxis,
					position: currentPosition,
				};
			}

			// Amount filter
			if (currentAmountMode !== "all") {
				if (currentAmountMode === "percent") {
					query.amount = { mode: "percent", value: currentAmountValue };
				} else if (currentAmountMode === "count") {
					query.amount = { mode: "count", value: currentAmountValue };
				} else if (currentAmountMode === "every_other") {
					query.amount = { mode: "every_other" };
				}
			} else {
				query.amount = { mode: "all" };
			}

			onChangeRef.current(query);
		},
		[
			xorTypes,
			fallbackTypes,
			spatialEnabled,
			axis,
			position,
			amountMode,
			amountValue,
		],
	);

	const addXorType = (type: FixtureType) => {
		if (!xorTypes.includes(type)) {
			const newTypes = [...xorTypes, type];
			setXorTypes(newTypes);
			notifyChange({ xorTypes: newTypes });
		}
	};

	const removeXorType = (type: FixtureType) => {
		const newTypes = xorTypes.filter((t) => t !== type);
		setXorTypes(newTypes);
		notifyChange({ xorTypes: newTypes });
	};

	const addFallbackType = (type: FixtureType) => {
		if (!fallbackTypes.includes(type)) {
			const newTypes = [...fallbackTypes, type];
			setFallbackTypes(newTypes);
			notifyChange({ fallbackTypes: newTypes });
		}
	};

	const removeFallbackType = (type: FixtureType) => {
		const newTypes = fallbackTypes.filter((t) => t !== type);
		setFallbackTypes(newTypes);
		notifyChange({ fallbackTypes: newTypes });
	};

	const handleSpatialEnabledChange = (enabled: boolean) => {
		setSpatialEnabled(enabled);
		notifyChange({ spatialEnabled: enabled });
	};

	const handleAxisChange = (newAxis: Axis) => {
		setAxis(newAxis);
		notifyChange({ axis: newAxis });
	};

	const handlePositionChange = (newPosition: AxisPosition) => {
		setPosition(newPosition);
		notifyChange({ position: newPosition });
	};

	const handleAmountModeChange = (
		newMode: "all" | "percent" | "count" | "every_other",
	) => {
		setAmountMode(newMode);
		notifyChange({ amountMode: newMode });
	};

	const handleAmountValueChange = (newValue: number) => {
		setAmountValue(newValue);
		notifyChange({ amountValue: newValue });
	};

	return (
		<div className="space-y-4 p-2">
			{/* Type Filter */}
			<div className="space-y-2">
				<Label className="text-[10px] text-muted-foreground uppercase tracking-wider">
					Type Filter (XOR)
				</Label>
				<div className="flex flex-wrap gap-1">
					{xorTypes.map((type) => (
						<button
							key={type}
							type="button"
							className="flex items-center gap-1 px-2 py-0.5 text-xs bg-primary/20 text-primary rounded"
							onClick={() => removeXorType(type)}
						>
							{FIXTURE_TYPES.find((t) => t.value === type)?.label ?? type}
							<X size={10} />
						</button>
					))}
					<Select onValueChange={(v) => addXorType(v as FixtureType)}>
						<SelectTrigger className="h-6 w-20 text-xs">
							<Plus size={10} />
						</SelectTrigger>
						<SelectContent>
							{FIXTURE_TYPES.filter((t) => !xorTypes.includes(t.value)).map(
								(type) => (
									<SelectItem key={type.value} value={type.value}>
										{type.label}
									</SelectItem>
								),
							)}
						</SelectContent>
					</Select>
				</div>
				<p className="text-[9px] text-muted-foreground">
					Randomly choose one type if multiple available
				</p>
			</div>

			{/* Fallback Types */}
			<div className="space-y-2">
				<Label className="text-[10px] text-muted-foreground uppercase tracking-wider">
					Fallback Types
				</Label>
				<div className="flex flex-wrap gap-1">
					{fallbackTypes.map((type) => (
						<button
							key={type}
							type="button"
							className="flex items-center gap-1 px-2 py-0.5 text-xs bg-muted text-muted-foreground rounded"
							onClick={() => removeFallbackType(type)}
						>
							{FIXTURE_TYPES.find((t) => t.value === type)?.label ?? type}
							<X size={10} />
						</button>
					))}
					<Select onValueChange={(v) => addFallbackType(v as FixtureType)}>
						<SelectTrigger className="h-6 w-20 text-xs">
							<Plus size={10} />
						</SelectTrigger>
						<SelectContent>
							{FIXTURE_TYPES.filter(
								(t) => !fallbackTypes.includes(t.value),
							).map((type) => (
								<SelectItem key={type.value} value={type.value}>
									{type.label}
								</SelectItem>
							))}
						</SelectContent>
					</Select>
				</div>
				<p className="text-[9px] text-muted-foreground">
					Try these in order if XOR types unavailable
				</p>
			</div>

			{/* Spatial Filter */}
			<div className="space-y-2">
				<div className="flex items-center gap-2">
					<input
						type="checkbox"
						id="spatial-enabled"
						checked={spatialEnabled}
						onChange={(e) => handleSpatialEnabledChange(e.target.checked)}
						className="rounded"
					/>
					<Label
						htmlFor="spatial-enabled"
						className="text-[10px] text-muted-foreground uppercase tracking-wider"
					>
						Spatial Filter
					</Label>
				</div>
				{spatialEnabled && (
					<div className="flex gap-2">
						<Select
							value={axis}
							onValueChange={(v) => handleAxisChange(v as Axis)}
						>
							<SelectTrigger className="h-7 text-xs flex-1">
								<SelectValue />
							</SelectTrigger>
							<SelectContent>
								{AXES.map((a) => (
									<SelectItem key={a.value} value={a.value}>
										{a.label}
									</SelectItem>
								))}
							</SelectContent>
						</Select>
						<Select
							value={position}
							onValueChange={(v) => handlePositionChange(v as AxisPosition)}
						>
							<SelectTrigger className="h-7 text-xs flex-1">
								<SelectValue />
							</SelectTrigger>
							<SelectContent>
								{AXIS_POSITIONS.map((p) => (
									<SelectItem key={p.value} value={p.value}>
										{p.label}
									</SelectItem>
								))}
							</SelectContent>
						</Select>
					</div>
				)}
			</div>

			{/* Amount Filter */}
			<div className="space-y-2">
				<Label className="text-[10px] text-muted-foreground uppercase tracking-wider">
					Amount
				</Label>
				<div className="flex gap-2">
					<Select
						value={amountMode}
						onValueChange={(v) =>
							handleAmountModeChange(
								v as "all" | "percent" | "count" | "every_other",
							)
						}
					>
						<SelectTrigger className="h-7 text-xs flex-1">
							<SelectValue />
						</SelectTrigger>
						<SelectContent>
							<SelectItem value="all">All</SelectItem>
							<SelectItem value="percent">Percent</SelectItem>
							<SelectItem value="count">Count</SelectItem>
							<SelectItem value="every_other">Every Other</SelectItem>
						</SelectContent>
					</Select>
					{(amountMode === "percent" || amountMode === "count") && (
						<Input
							type="number"
							value={amountValue}
							onChange={(e) => handleAmountValueChange(Number(e.target.value))}
							className="h-7 text-xs w-20"
							min={amountMode === "percent" ? 0 : 1}
							max={amountMode === "percent" ? 100 : undefined}
						/>
					)}
				</div>
			</div>
		</div>
	);
}
