export type AppView =
	| { type: "welcome" }
	| { type: "pattern"; patternId: number; name: string }
	| { type: "trackEditor"; trackId: number; trackName: string }
	| { type: "universe" };
