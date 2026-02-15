import { invoke } from "@tauri-apps/api/core";
import { Loader2, SunDim } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { useAppViewStore } from "@/features/app/stores/use-app-view-store";
import { useFixtureStore } from "@/features/universe/stores/use-fixture-store";
import { StageVisualizer } from "@/features/visualizer/components/stage-visualizer";
import { usePerformStore } from "../stores/use-perform-store";
import { DeckDisplay } from "./deck-display";
import { SourceSelector } from "./source-selector";

export function PerformPage() {
	const connectionStatus = usePerformStore((s) => s.connectionStatus);
	const source = usePerformStore((s) => s.source);
	const deviceName = usePerformStore((s) => s.deviceName);
	const decks = usePerformStore((s) => s.decks);
	const crossfader = usePerformStore((s) => s.crossfader);
	const error = usePerformStore((s) => s.error);
	const connect = usePerformStore((s) => s.connect);
	const disconnect = usePerformStore((s) => s.disconnect);
	const deckMatches = usePerformStore((s) => s.deckMatches);
	const activeDeckId = usePerformStore((s) => s.activeDeckId);
	const isCompositing = usePerformStore((s) => s.isCompositing);
	const currentVenueId = useAppViewStore((s) => s.currentVenue?.id ?? null);
	const [darkStage, setDarkStage] = useState(true);

	// Initialize fixtures for the visualizer
	useEffect(() => {
		if (currentVenueId !== null) {
			useFixtureStore.getState().initialize(currentVenueId);
		} else {
			useFixtureStore.getState().initialize();
		}
	}, [currentVenueId]);

	// Cleanup on unmount — clear perform render state so track editor playback still works
	useEffect(() => {
		return () => {
			invoke("render_clear_perform").catch(() => {});
			const { connectionStatus } = usePerformStore.getState();
			if (
				connectionStatus === "connected" ||
				connectionStatus === "connecting"
			) {
				usePerformStore.getState().disconnect();
			}
		};
	}, []);

	// Compute render time from active deck
	const activeDeck = activeDeckId ? decks.get(activeDeckId) : null;
	const activeMatch = activeDeckId ? deckMatches.get(activeDeckId) : null;
	const renderAudioTimeSec = useMemo(() => {
		if (activeMatch?.hasLightShow && activeDeck && activeDeck.sample_rate > 0) {
			return activeDeck.samples / activeDeck.sample_rate;
		}
		return null;
	}, [activeDeck, activeMatch?.hasLightShow]);

	// Check if any deck has a light show
	const hasAnyLightShow = useMemo(() => {
		for (const match of deckMatches.values()) {
			if (match.hasLightShow) return true;
		}
		return false;
	}, [deckMatches]);

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

	// Connected — show decks + visualizer
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
					{isCompositing && (
						<Loader2 className="w-3 h-3 animate-spin text-muted-foreground" />
					)}
					<button
						type="button"
						onClick={() => setDarkStage((v) => !v)}
						className={`inline-flex items-center gap-1 px-2 py-1 rounded text-[10px] transition-colors ${
							darkStage
								? "bg-primary/20 text-primary"
								: "text-muted-foreground hover:text-foreground"
						}`}
						title="Toggle dark stage"
					>
						<SunDim className="w-3 h-3" />
						{darkStage ? "Dark" : "Lit"}
					</button>
					<div className="h-1.5 w-1.5 rounded-full bg-green-500" />
					<span className="text-[10px] text-muted-foreground">Connected</span>
				</div>
			</div>

			{/* Visualizer */}
			{hasAnyLightShow && (
				<div className="flex-1 min-h-0 relative">
					<StageVisualizer
						enableEditing={false}
						renderAudioTimeSec={renderAudioTimeSec}
						darkStage={darkStage}
					/>
				</div>
			)}

			{/* Deck displays */}
			<div
				className={`flex gap-4 p-4 min-h-0 ${hasAnyLightShow ? "" : "flex-1"}`}
			>
				{activeDeck1 ? (
					<DeckDisplay
						deck={activeDeck1}
						matchState={deckMatches.get(1)}
						isActiveDeck={activeDeckId === 1}
					/>
				) : (
					<DeckPlaceholder id={1} />
				)}
				{activeDeck2 ? (
					<DeckDisplay
						deck={activeDeck2}
						matchState={deckMatches.get(2)}
						isActiveDeck={activeDeckId === 2}
					/>
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
