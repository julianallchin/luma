import type {
	ScoreDslDiagnostic as ScoreDslDiagnosticBinding,
	ScoreDslExportResponse,
	ScoreDslImportResponse,
	ScoreDslValidationResponse,
} from "@/bindings/schema";
import { invoke } from "@/shared/lib/tauri";

export type ScoreDslScope = {
	scoreId: string;
	trackId: string;
	venueId: string;
};

export type ScoreDslExport = ScoreDslExportResponse;
export type ScoreDslDiagnostic = ScoreDslDiagnosticBinding;
export type ScoreDslValidation = ScoreDslValidationResponse;
export type ScoreDslImport = ScoreDslImportResponse;

export function exportScoreDsl(
	scope: ScoreDslScope,
	includeClipIds: boolean,
): Promise<ScoreDslExport> {
	return invoke<ScoreDslExport>("score_dsl_export", {
		...scope,
		includeClipIds,
	});
}

export function validateScoreDsl(
	scope: ScoreDslScope,
	source: string,
): Promise<ScoreDslValidation> {
	return invoke<ScoreDslValidation>("score_dsl_validate", {
		...scope,
		source,
	});
}

export function importScoreDsl(
	scope: ScoreDslScope,
	source: string,
	baseRevision: string,
): Promise<ScoreDslImport> {
	const request = {
		...scope,
		source,
		baseRevision,
		operationId: crypto.randomUUID(),
	};
	return invoke<ScoreDslImport>("score_dsl_import", request).catch(() =>
		invoke<ScoreDslImport>("score_dsl_import", request),
	);
}
