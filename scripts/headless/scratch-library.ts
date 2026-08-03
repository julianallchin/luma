import { Database } from "bun:sqlite";

type SchemaObject = { name: string; sql: string };
type TableName = { name: string };

/**
 * Re-home the authored roots in a copied library for a headless fixture. This
 * must only be called for a disposable copy: changing principal identity
 * intentionally starts new relational history and clears copied
 * thread/control-plane records, while preserving the live projections under
 * test.
 *
 * A source library may already contain the immutable-identity triggers this
 * branch introduces. Removing and restoring its user-defined triggers inside
 * the fixture transaction models an import instead of punching a maintenance
 * hole through production identity rules.
 */
export function normalizeScratchLibraryToGuest(databasePath: string): void {
	normalizeScratchLibraryOwnership(databasePath, null);
}

/** Re-home a disposable library under a synthetic authenticated owner. The
 * headless harness receives the same id as a trusted fixture principal, so
 * mutation authority can be exercised without copying live auth secrets. */
export function normalizeScratchLibraryToPrincipal(
	databasePath: string,
	principal: string,
): void {
	if (!principal.trim()) throw new Error("scratch principal cannot be empty");
	normalizeScratchLibraryOwnership(databasePath, principal);
}

function normalizeScratchLibraryOwnership(
	databasePath: string,
	principal: string | null,
): void {
	const database = new Database(databasePath);
	try {
		const triggers = database
			.query<SchemaObject, []>(
				`SELECT name, sql
				 FROM sqlite_master
				 WHERE type = 'trigger' AND sql IS NOT NULL
				 ORDER BY name`,
			)
			.all();
		const controlTables = database
			.query<TableName, []>(
				`SELECT name
				 FROM sqlite_master
				 WHERE type = 'table'
				   AND (
				     (name GLOB 'authored_*' AND name <> 'authored_device_identity')
				     OR name GLOB 'agent_thread*'
				     OR name = 'pending_ops'
				   )
				 ORDER BY name`,
			)
			.all();

		database.exec("PRAGMA foreign_keys = OFF");
		database.exec("BEGIN IMMEDIATE");
		try {
			for (const trigger of triggers) {
				database.exec(`DROP TRIGGER ${quoteIdentifier(trigger.name)}`);
			}
			for (const table of controlTables) {
				database.exec(`DELETE FROM ${quoteIdentifier(table.name)}`);
			}

			for (const table of ["patterns", "implementations", "scores", "track_scores", "tracks"]) {
				if (tableExists(database, table)) {
					database
						.query(`UPDATE ${quoteIdentifier(table)} SET uid = ?`)
						.run(principal);
				}
			}
			if (tableExists(database, "venues")) {
				database
					.query("UPDATE venues SET uid = ?, role = 'owner'")
					.run(principal);
			}

			for (const trigger of triggers) database.exec(trigger.sql);
			database.exec("COMMIT");
		} catch (error) {
			database.exec("ROLLBACK");
			throw error;
		} finally {
			database.exec("PRAGMA foreign_keys = ON");
		}
	} finally {
		database.close();
	}
}

function tableExists(database: Database, table: string): boolean {
	return (
		database
			.query<{ present: number }, [string]>(
				"SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?) AS present",
			)
			.get(table)?.present === 1
	);
}

function quoteIdentifier(identifier: string): string {
	return `"${identifier.replaceAll('"', '""')}"`;
}
