import { type ClassValue, clsx } from "clsx";
import { twMerge } from "tailwind-merge";

export function cn(...inputs: ClassValue[]) {
	return twMerge(clsx(inputs));
}

/**
 * Converts a string to snake_case (like a Python function name).
 * - Lowercase only
 * - Replaces spaces and special characters with underscores
 * - No consecutive underscores
 * - No leading/trailing underscores
 * - Only alphanumeric and underscores allowed
 */
export function toSnakeCase(input: string): string {
	return input
		.trim()
		.toLowerCase()
		.replace(/[^a-z0-9]+/g, "_") // Replace non-alphanumeric with underscore
		.replace(/_+/g, "_") // Collapse consecutive underscores
		.replace(/^_|_$/g, ""); // Remove leading/trailing underscores
}
