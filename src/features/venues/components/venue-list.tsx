import { useEffect, useId } from "react";
import { useNavigate } from "react-router-dom";
import type { Venue } from "@/bindings/venues";
import { useVenuesStore } from "../stores/use-venues-store";

export function VenueList() {
	const { venues, loading, error, refresh } = useVenuesStore();
	const navigate = useNavigate();
	const instanceId = useId();

	useEffect(() => {
		refresh();
	}, [refresh]);

	const handleVenueClick = (venue: Venue) => {
		navigate(`/venue/${venue.id}/universe`);
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
	const emptySlotIds = Array.from({ length: emptySlots }, (_, i) =>
		`${instanceId}-empty-${i}`,
	);

	return (
		<div className="grid grid-rows-2 grid-cols-3 gap-4 w-2xl">
			{displayVenues.map((venue) => (
				<button
					key={venue.id}
					type="button"
					onClick={() => handleVenueClick(venue)}
					className="bg-input border h-36 rounded-md p-4 flex flex-col justify-between text-left hover:bg-muted transition-colors cursor-pointer"
				>
					<div>
						<h3 className="font-medium text-sm truncate">{venue.name}</h3>
						{venue.description && (
							<p className="text-xs text-muted-foreground mt-1 line-clamp-2">
								{venue.description}
							</p>
						)}
					</div>
					<div className="text-[10px] text-muted-foreground">
						{new Date(venue.updatedAt).toLocaleDateString()}
					</div>
				</button>
			))}
			{emptySlotIds.map((id) => (
				<div
					key={id}
					className="bg-input/30 border border-dashed h-36 rounded-md"
				/>
			))}
		</div>
	);
}
