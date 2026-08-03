import { createContext, useContext } from "react";

export const SubagentNavContext = createContext<
	((subagentId: string) => void) | null
>(null);

export function useSubagentNav() {
	return useContext(SubagentNavContext);
}
