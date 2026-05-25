import { CreateVenueDialog } from "@/features/venues/components/create-venue-dialog";
import { JoinVenueDialog } from "@/features/venues/components/join-venue-dialog";
import { VenueList } from "@/features/venues/components/venue-list";
import { Button } from "@/shared/components/ui/button";

export function WelcomeScreen() {
	return (
		<div className="relative h-full w-full bg-background text-foreground">
			<div className="absolute top-1/2 left-1/2 -translate-x-1/2 -translate-y-1/2 flex flex-col items-center gap-8">
				<h1 className="text-6xl font-extralight tracking-[0.2em] opacity-80 select-none">
					luma
				</h1>

				<VenueList />

				<div className="flex flex-col gap-2 w-64 z-10">
					<CreateVenueDialog
						trigger={<Button className="w-full">new venue</Button>}
					/>
					<JoinVenueDialog
						trigger={<Button className="w-full">join venue</Button>}
					/>
				</div>

				<div className="absolute top-full left-1/2 -translate-x-1/2 mt-12 w-80" />
			</div>
		</div>
	);
}
