import { z } from "zod";
import type { FixtureFacing, PatchedFixture } from "@/bindings/fixtures";
import type { FixtureGroupNode } from "@/bindings/groups";
import type { ResolvedNode, ResolvedVenue } from "@/bindings/venue-graph";
import { VENUE_EXPERT_MODEL } from "@/features/track-editor/agent/openrouter-key";
import { tool } from "@/shared/lib/agent/agent-tool";
import { lumaPiModel } from "@/shared/lib/agent/openrouter";
import { completePiText } from "@/shared/lib/agent/pi-agent-loop";
import { invoke } from "@/shared/lib/tauri";

/**
 * The shared "ask the venue expert" tool, used by both the track and graph
 * agents. It dumps every fixture + group of a venue to a venue-expert model and
 * returns prose grounding a lighting decision in the actual rig.
 *
 * The only per-agent difference is how the current venue id is resolved, so the
 * caller passes a `getVenueId` resolver.
 */
export function buildAskVenueTool({
	getVenueId,
}: {
	getVenueId: () => string | null;
}) {
	return tool({
		description: `Ask a venue expert a natural-language question about this venue's physical setup. The expert knows every fixture (id, name, type — par/led_bar/moving_head/etc., xyz position in meters, facing direction) and every group (snake_case name + which fixtures it contains). Use it whenever you need to ground a lighting decision in the actual rig — don't guess at group names.

Use ask_venue to:
- Discover what groups exist ("what groups are there for the front wash?", "is there a group of only uplighters?")
- Pick a selection expression for a role ("which groups make a good foundation wash?", "which groups are movers on the back wall?")
- Resolve spatial intents ("which groups are pointing up?", "what's on stage left?")

The expert returns plain prose. Quote the exact snake_case group names from the answer when building a selection. Ask focused questions — one intent per call.`,
		inputSchema: z.object({
			question: z
				.string()
				.describe(
					"A single, focused question about the venue's fixtures or groups.",
				),
		}),
		execute: async ({ question }) => {
			const venueId = getVenueId();
			if (!venueId) {
				return { error: "No venue loaded; cannot consult the venue expert." };
			}
			const runtime = lumaPiModel(VENUE_EXPERT_MODEL);
			if (!runtime) {
				return { error: "The selected AI provider has no API key." };
			}

			let fixtures: PatchedFixture[];
			let facings: FixtureFacing[];
			let groups: FixtureGroupNode[];
			let venue: ResolvedVenue;
			try {
				[fixtures, facings, groups, venue] = await Promise.all([
					invoke<PatchedFixture[]>("get_patched_fixtures", { venueId }),
					invoke<FixtureFacing[]>("get_fixture_facings", { venueId }),
					invoke<FixtureGroupNode[]>("get_grouped_hierarchy", { venueId }),
					invoke<ResolvedVenue>("get_resolved_venue", { venueId }),
				]);
			} catch (err) {
				return { error: `Failed to load venue data: ${String(err)}` };
			}

			const venueDump = formatVenueContext(
				fixtures,
				facings,
				groups,
				new Map(venue.nodes.map((n) => [n.id, n])),
			);

			try {
				const answer = await completePiText({
					runtime,
					systemPrompt: `You are a venue expert for a lighting design tool. You receive a dump of every fixture and every group in a venue, then answer one question from another agent who is composing a lighting score.

Coordinate system: Z-up, meters. +X = stage right, +Y = back of venue, -Y = audience/front, +Z = up. Facing labels (up/down/house/upstage/stage-left/stage-right) name the direction a parked fixture's beam leaves along — the outward normal of whatever it is mounted on. Raw Euler angles in radians are also given for precision.

Fixture types: par_wash (basic wash light), pixel_bar (linear pixel strip), moving_head (motorized beam), scanner, strobe, static, unknown.

Answer in plain prose. Always quote the exact snake_case group names so the caller can paste them into a Selection expression. If the venue has nothing matching the question, say so plainly — don't fabricate groups. If the question is ambiguous, give the best literal read and note the ambiguity. Be terse: the caller is mid-task and just needs the answer.`,
					prompt: `${venueDump}\n\nQuestion: ${question}`,
				});
				return { answer };
			} catch (err) {
				return { error: `Venue expert call failed: ${String(err)}` };
			}
		},
	});
}

/**
 * Compact text dump of every fixture + every group for the venue expert.
 * Designed to fit comfortably in context for a typical venue.
 */
export function formatVenueContext(
	fixtures: PatchedFixture[],
	facings: FixtureFacing[],
	groups: FixtureGroupNode[],
	/** Solved poses by fixture id. A fixture absent here is patched but unplaced. */
	poses: Map<string, ResolvedNode>,
): string {
	const facingWord = new Map(facings.map((f) => [f.id, f.word]));
	const fixtureLabel = new Map<string, string>();
	for (const f of fixtures) {
		fixtureLabel.set(f.id, f.label ?? `${f.manufacturer} ${f.model}`);
	}

	const fixtureLines = fixtures.map((f) => {
		const name = fixtureLabel.get(f.id) ?? "<unnamed>";
		const node = poses.get(f.id);
		const facing = facingWord.get(f.id) ?? "?";
		const type = inferRoleFromGroups(f.id, groups);
		if (!node) {
			return `${f.id} | "${name}" | ${type} | unplaced (patched, not yet on the stage)`;
		}
		const pos = `(${node.position.map(fmtN).join(", ")})`;
		const rot = `(${node.rotation.map(fmtN).join(", ")})`;
		return `${f.id} | "${name}" | ${type} | pos=${pos}m | facing=${facing} | rot=${rot}rad`;
	});

	const groupLines = groups.map((g) => {
		const name = g.name || "<unnamed>";
		const members = g.fixtures
			.map((m) => {
				const headCount = Number(m.headCount);
				const partial = headCount > 0 && m.heads.length < headCount;
				const headsStr = partial
					? `, heads ${m.heads.map((h) => Number(h.headIndex) + 1).join(",")} of ${headCount}`
					: "";
				return `${m.id} ("${m.label}"${headsStr})`;
			})
			.join(", ");
		const axes: string[] = [];
		// Absent on a derived set — it has no authored row to carry them.
		if (g.axisLr != null) axes.push(`LR=${fmtN(g.axisLr)}`);
		if (g.axisFb != null) axes.push(`FB=${fmtN(g.axisFb)}`);
		if (g.axisAb != null) axes.push(`AB=${fmtN(g.axisAb)}`);
		const axesStr = axes.length > 0 ? `  [${axes.join(" ")}]` : "";
		// A derived node names its role branch; a hand-made group has none, so
		// it says where it came from instead.
		return `${name} (${g.role ?? g.origin}, ${g.fixtures.length} fixtures)${axesStr}\n    ${members || "<empty>"}`;
	});

	return `# Venue context

## Fixtures (${fixtures.length})
Format: id | name | type | position (x,y,z meters) | facing | rotation (x,y,z radians). A fixture with no position is patched but not yet placed on the stage.
${fixtureLines.join("\n")}

## Groups (${groups.length})
Format: snake_case_name (type, count)  [optional axis positions, normalized −1..+1]
    members listed as id ("label")
${groupLines.join("\n")}

## Selection expressions
Selections reference group names with boolean ops: \`front_wash\`, \`front_wash & left_movers\`, \`drum_uplighters | dj_booth > back_wash\`. The literal \`all\` selects every fixture.`;
}

function fmtN(n: number): string {
	return Number.isFinite(n) ? n.toFixed(2) : "?";
}

/**
 * Find which group a fixture belongs to to read its role, since
 * PatchedFixture doesn't carry the derived FixtureRole directly.
 */
function inferRoleFromGroups(
	fixtureId: string,
	groups: FixtureGroupNode[],
): string {
	for (const g of groups) {
		for (const f of g.fixtures) {
			if (f.id === fixtureId) return f.role;
		}
	}
	return "other";
}
