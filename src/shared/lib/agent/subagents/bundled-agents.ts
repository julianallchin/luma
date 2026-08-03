import generalPurpose from "./agents/general-purpose.md?raw";

/** Generic defaults. Product-specific agent types are injected at runtime. */
export const BUNDLED_AGENT_DEFINITIONS = [generalPurpose] as const;
