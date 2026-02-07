import { useEffect } from "react";
import { usePerformStore } from "../stores/use-perform-store";
import { DeckDisplay } from "./deck-display";
import { SourceSelector } from "./source-selector";

export function PerformPage() {
	const {
		connectionStatus,
		source,
		deviceName,
		decks,
		crossfader,
		error,
		connect,
		disconnect,
	} = usePerformStore();

	// Cleanup on unmount
	useEffect(() => {
		return () => {
			const { connectionStatus } = usePerformStore.getState();
			if (
				connectionStatus === "connected" ||
				connectionStatus === "connecting"
			) {
				usePerformStore.getState().disconnect();
			}
		};
	}, []);

	// Source selection screen
	if (!source) {
		return <SourceSelector onSelect={connect} />;
	}

	// Connecting / searching
	if (connectionStatus === "connecting") {
		return (
			<div className="flex flex-col items-center justify-center h-full gap-3">
				<div className="text-sm text-muted-foreground">
					Searching for StageLinQ devices...
				</div>
				<button
					type="button"
					onClick={disconnect}
					className="text-xs text-muted-foreground hover:text-foreground transition-colors"
				>
					cancel
				</button>
			</div>
		);
	}

	// Error state
	if (connectionStatus === "error") {
		return (
			<div className="flex flex-col items-center justify-center h-full gap-3">
				<div className="text-sm text-destructive">{error}</div>
				<div className="flex gap-3">
					<button
						type="button"
						onClick={() => connect("stagelinq")}
						className="text-xs text-muted-foreground hover:text-foreground transition-colors"
					>
						retry
					</button>
					<button
						type="button"
						onClick={disconnect}
						className="text-xs text-muted-foreground hover:text-foreground transition-colors"
					>
						back
					</button>
				</div>
			</div>
		);
	}

	// Connected — show decks
	const deckArray = Array.from(decks.values());
	const activeDeck1 = deckArray.find((d) => d.id === 1);
	const activeDeck2 = deckArray.find((d) => d.id === 2);

	return (
		<div className="flex flex-col h-full">
			{/* Header bar */}
			<div className="flex items-center justify-between px-4 py-2 border-b border-border/40">
				<div className="flex items-center gap-3">
					<button
						type="button"
						onClick={disconnect}
						className="text-xs text-muted-foreground hover:text-foreground transition-colors"
					>
						&larr; sources
					</button>
					{deviceName && (
						<span className="text-xs text-muted-foreground">{deviceName}</span>
					)}
				</div>
				<div className="flex items-center gap-2">
					<div className="h-1.5 w-1.5 rounded-full bg-green-500" />
					<span className="text-[10px] text-muted-foreground">Connected</span>
				</div>
			</div>

			{/* Deck displays */}
			<div className="flex-1 flex gap-4 p-4 min-h-0">
				{activeDeck1 ? (
					<DeckDisplay deck={activeDeck1} />
				) : (
					<DeckPlaceholder id={1} />
				)}
				{activeDeck2 ? (
					<DeckDisplay deck={activeDeck2} />
				) : (
					<DeckPlaceholder id={2} />
				)}
			</div>

			{/* Crossfader */}
			<div className="px-4 pb-4">
				<div className="flex items-center gap-2">
					<span className="text-[10px] text-muted-foreground uppercase tracking-wider w-16">
						Crossfader
					</span>
					<div className="h-1 bg-muted-foreground/10 flex-1 relative">
						<div
							className="absolute top-1/2 -translate-y-1/2 w-2 h-3 bg-foreground/60"
							style={{ left: `${(crossfader * 100).toFixed(0)}%` }}
						/>
					</div>
				</div>
			</div>
		</div>
	);
}

function DeckPlaceholder({ id }: { id: number }) {
	return (
		<div className="border border-border/40 bg-background/50 p-4 flex-1 flex items-center justify-center">
			<span className="text-xs text-muted-foreground">
				Deck {id} — waiting for data
			</span>
		</div>
	);
}
