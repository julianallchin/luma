export type AppView =
	| { type: "welcome" }
	| { type: "pattern"; patternId: number; name: string };
