import { Settings2 } from "lucide-react";
import { useEffect, useId, useState } from "react";
import { useNavigate } from "react-router-dom";
import type { Venue } from "@/bindings/venues";
import { useVenuesStore } from "../stores/use-venues-store";
import { VenueSettingsDialog } from "./venue-settings-dialog";

export function VenueList() {
	const { venues, loading, error, refresh } = useVenuesStore();
	const navigate = useNavigate();
	const instanceId = useId();
	const [settingsVenue, setSettingsVenue] = useState<Venue | null>(null);

	useEffect(() => {
		refresh();
	}, [refresh]);

	const handleVenueClick = (venue: Venue) => {
		navigate(`/venue/${venue.id}/edit`);
	};

	const placeholderIds = [
		`${instanceId}-0`,
		`${instanceId}-1`,
		`${instanceId}-2`,
		`${instanceId}-3`,
		`${instanceId}-4`,
		`${instanceId}-5`,
	];

	if (loading && venues.length === 0) {
		return (
			<div className="grid grid-rows-2 grid-cols-3 gap-4 w-2xl">
				{placeholderIds.map((id) => (
					<div
						key={id}
						className="bg-input border h-36 animate-pulse rounded-md"
					/>
				))}
			</div>
		);
	}

	if (error) {
		return (
			<div className="text-destructive text-sm p-4 bg-destructive/10 rounded-md">
				Failed to load venues: {error}
			</div>
		);
	}

	// Show empty placeholder grid if no venues
	if (venues.length === 0) {
		return (
			<div className="grid grid-rows-2 grid-cols-3 gap-4 w-2xl">
				{placeholderIds.map((id, i) => (
					<div
						key={id}
						className="bg-input/50 border border-dashed h-36 rounded-md flex items-center justify-center"
					>
						<span className="text-muted-foreground text-xs">
							{i === 0 ? "Create your first venue" : ""}
						</span>
					</div>
				))}
			</div>
		);
	}

	// Show venues in a grid (up to 6, then it will need pagination)
	const displayVenues = venues.slice(0, 6);
	const emptySlots = Math.max(0, 6 - displayVenues.length);
	const emptySlotIds = Array.from(
		{ length: emptySlots },
		(_, i) => `${instanceId}-empty-${i}`,
	);

	return (
		<>
			<div className="grid grid-rows-2 grid-cols-3 gap-4 w-2xl">
				{displayVenues.map((venue) => (
					// biome-ignore lint/a11y/useSemanticElements: styled card with nested buttons
					<div
						key={venue.id}
						className="group relative bg-input border h-36 rounded-md p-4 flex flex-col justify-between text-left hover:bg-muted transition-colors cursor-pointer"
						role="button"
						tabIndex={0}
						onClick={() => handleVenueClick(venue)}
						onKeyDown={(e) => {
							if (e.key === "Enter" || e.key === " ") {
								handleVenueClick(venue);
							}
						}}
					>
						{venue.role === "owner" && (
							<button
								type="button"
								className="absolute top-2 right-2 p-1 rounded opacity-0 group-hover:opacity-100 hover:bg-foreground/10 transition-opacity"
								onClick={(e) => {
									e.stopPropagation();
									setSettingsVenue(venue);
								}}
							>
								<Settings2 className="size-3.5 text-muted-foreground" />
							</button>
						)}
						<div>
							<div className="flex items-center gap-2">
								<h3 className="font-medium text-sm truncate">{venue.name}</h3>
								{venue.role === "member" && (
									<span className="text-[9px] px-1.5 py-0.5 rounded bg-muted-foreground/10 text-muted-foreground shrink-0">
										joined
									</span>
								)}
							</div>
							{venue.description && (
								<p className="text-xs text-muted-foreground mt-1 line-clamp-2">
									{venue.description}
								</p>
							)}
						</div>
						<div className="text-[10px] text-muted-foreground">
							{new Date(venue.updatedAt).toLocaleDateString()}
						</div>
					</div>
				))}
				{emptySlotIds.map((id) => (
					<div
						key={id}
						className="bg-input/30 border border-dashed h-36 rounded-md"
					/>
				))}
			</div>

			{settingsVenue && (
				<VenueSettingsDialog
					venue={settingsVenue}
					open={!!settingsVenue}
					onOpenChange={(open) => {
						if (!open) setSettingsVenue(null);
					}}
				/>
			)}
		</>
	);
}
