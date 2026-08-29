//! DMX addressing: one allocator, one rule.
//!
//! A rig is *built*, never typed, so an address is **derived from where the
//! fixture hangs** rather than from the order somebody happened to add it.
//! The rule, whole:
//!
//! - A fixture's universe is **its run's universe**. One universe per run
//!   (`docs/specs/venue-builder-gauntlet.md` §3), so a data fault — a dead node,
//!   a pulled cable — is bounded to one structure, which is how real rigs are
//!   patched and why Vectorworks Spotlight numbers by plot position rather than
//!   by creation order.
//! - Within a run, addresses are **consecutive in physical order along the
//!   run**: `along(t)` ascending, ties broken by node id so the answer is
//!   byte-stable.
//! - A run whose block will not fit in its candidate universe **rolls whole**
//!   to the next one — it never splits across the boundary — and says so
//!   ([`Note::RunRolled`]). Only a run whose own footprint exceeds a universe
//!   has to split, and that says so too ([`Note::RunSplit`]).
//! - A **pinned** fixture — an address a human set by hand — is never
//!   re-derived. It is reserved first, and the derived blocks flow around it.
//! - A fixture with no run — unplaced in the tray, or resting on the floor
//!   rather than on structure — takes the next free slot from universe 1,
//!   filling the gaps the run blocks left, in fixture-id order.
//!
//! # Why this module has no database and no catalog
//!
//! Allocation is a *function of the solved venue*. [`allocate`] takes a
//! [`ResolvedVenue`] and a fixture list and returns assignments; it reads no
//! rows, writes none, and cannot fail. Everything that can fail — a hand-set
//! address that collides, a footprint past the end of a universe — is refused
//! at the one place a human types a number, and is expressed by
//! [`Footprint::new`] returning `None` and [`Occupancy::conflict`] naming the
//! fixture in the way.

use std::collections::BTreeMap;

use crate::venue::{NodeKind, ResolvedVenue};

/// Channels in one DMX universe. Addresses are `1..=512`, so a fixture of `n`
/// channels at address `a` occupies `a ..= a + n - 1` and needs
/// `a + n - 1 <= 512`.
pub const UNIVERSE_SIZE: u16 = 512;

/// Where a fixture sits in the patch, as a range that is known to be inside one
/// universe.
///
/// Constructed only through [`Footprint::new`], which is the *only* range check
/// in the system: a `Footprint` that exists is addressable, so nothing
/// downstream — not the allocator, not the DMX engine — has a truncation branch
/// to take.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct Footprint {
    universe: u16,
    first: u16,
    channels: u16,
}

impl Footprint {
    /// The footprint of `channels` channels starting at `address` in
    /// `universe`, or `None` if that does not fit in `1..=`[`UNIVERSE_SIZE`].
    ///
    /// A zero-channel fixture is refused as well: it has no footprint, so it
    /// could neither collide nor be collided with, and a patch row that cannot
    /// collide is a row that silently overlaps everything.
    #[must_use]
    pub fn new(universe: u16, address: u16, channels: u16) -> Option<Footprint> {
        if channels == 0 || address == 0 {
            return None;
        }
        if address.checked_add(channels - 1)? > UNIVERSE_SIZE {
            return None;
        }
        Some(Footprint {
            universe,
            first: address,
            channels,
        })
    }

    #[must_use]
    pub fn universe(&self) -> u16 {
        self.universe
    }

    /// The first channel, `1..=512`.
    #[must_use]
    pub fn address(&self) -> u16 {
        self.first
    }

    #[must_use]
    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// The last channel, `1..=512`.
    #[must_use]
    pub fn last(&self) -> u16 {
        self.first + self.channels - 1
    }

    /// Whether the two claim any channel in common.
    #[must_use]
    pub fn overlaps(&self, other: &Footprint) -> bool {
        self.universe == other.universe && self.first <= other.last() && other.first <= self.last()
    }

    /// Whether this footprint covers `address` in `universe`.
    #[must_use]
    pub fn covers(&self, universe: u16, address: u16) -> bool {
        self.universe == universe && (self.first..=self.last()).contains(&address)
    }
}

/// What a patch row already says about where a fixture lives, and how firmly.
///
/// One field with three states rather than an address beside a flag: a pin
/// *is* an address, and "pinned but nowhere" is not a patch anybody can write,
/// so it should not be a value anybody can construct.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Address {
    /// No row yet — a fixture a distribution or the add dialog is about to
    /// create, which is why it is asking where it would go.
    #[default]
    Unset,
    /// Where the last allocation left it. Free to be re-derived, but *stored*:
    /// until the next auto-patch these are the channels the database says are
    /// taken.
    Derived(Footprint),
    /// Where a human put it by hand. Never re-derived; reserved first.
    Pinned(Footprint),
}

impl Address {
    /// The channels the row holds right now, pinned or not — the stored patch.
    #[must_use]
    pub fn footprint(self) -> Option<Footprint> {
        match self {
            Address::Unset => None,
            Address::Derived(footprint) | Address::Pinned(footprint) => Some(footprint),
        }
    }

    /// The footprint the allocator must leave alone.
    #[must_use]
    pub fn pin(self) -> Option<Footprint> {
        match self {
            Address::Pinned(footprint) => Some(footprint),
            Address::Unset | Address::Derived(_) => None,
        }
    }
}

/// A fixture as the allocator sees it: an identity, a width, and the row's own
/// account of where it currently is.
#[derive(Clone, Debug)]
pub struct Fixture {
    /// The patch row id. A fixture's venue node carries the same id
    /// (`crate::venue_graph::migrate`), which is how a placement is found.
    pub id: String,
    /// The mode's channel count.
    pub channels: u16,
    /// The stored address. [`allocate`] honours only its pins;
    /// [`next_addresses`] honours all of it, because a stored address is what
    /// the row *collides with* until something moves it.
    pub address: Address,
}

/// One fixture's place in the patch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Assignment {
    pub fixture: String,
    pub footprint: Footprint,
    /// The run this fixture hangs from, and therefore whose universe it took.
    /// `None` for a fixture in the tray or resting on the floor.
    pub run: Option<String>,
    /// Whether this is a hand-set address the allocator preserved rather than
    /// derived. `auto_patch` writes only the rest.
    pub pinned: bool,
}

/// Something the allocation had to decide, worth telling the human.
///
/// Warnings rather than errors: a rig that outgrows a universe is a real rig,
/// not a mistake, and refusing to patch it would leave the fixtures with no
/// address at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Note {
    /// The run did not fit in the universe it was offered, so the **whole**
    /// block moved on rather than straddling the boundary.
    RunRolled {
        run: String,
        offered: u16,
        taken: u16,
    },
    /// The run's own footprint is wider than a universe, so it is the one case
    /// that must span several.
    RunSplit { run: String, universes: Vec<u16> },
    /// There was nowhere left: every universe from the fixture's starting point
    /// up to [`MAX_UNIVERSE`] was full. The fixture has no assignment.
    NoRoom { fixture: String },
}

/// How far the allocator will roll before giving up. Art-Net's port address is
/// 15 bits; a rig that has walked past 512 universes has a bug, not a truss.
pub const MAX_UNIVERSE: u16 = 32_767;

/// What one allocation decided.
#[derive(Clone, Debug, Default)]
pub struct Allocation {
    /// Every fixture that got a place, in the order the rule decided them:
    /// **pins first**, in input order, because they are reserved before
    /// anything is derived; then each run in solve order, its fixtures in
    /// along-the-run order; then the run-less ones, by fixture id.
    pub assignments: Vec<Assignment>,
    pub notes: Vec<Note>,
}

impl Allocation {
    /// One fixture's assignment, by patch-row id.
    #[must_use]
    pub fn get(&self, fixture: &str) -> Option<&Assignment> {
        self.assignments.iter().find(|a| a.fixture == fixture)
    }
}

// ---------------------------------------------------------------------------
// Occupancy
// ---------------------------------------------------------------------------

/// Which channels are taken, and by whom.
///
/// The single source for every collision question: the allocator flows its
/// blocks around it, `set_address` asks it whether a typed address is free, and
/// the patch page's footprint strip is drawn from it. A second implementation
/// of "is this address free" is the bug this type exists to make impossible.
#[derive(Clone, Debug, Default)]
pub struct Occupancy {
    /// Per universe, the claims in that universe sorted by first channel.
    universes: BTreeMap<u16, Vec<(Footprint, String)>>,
}

impl Occupancy {
    /// The occupancy of a set of already-addressed fixtures.
    ///
    /// Overlapping claims are *kept*, not resolved: a database written before
    /// anything validated can hold them, and the strip has to show them in red
    /// rather than hide one of them.
    #[must_use]
    pub fn of(claims: impl IntoIterator<Item = (Footprint, String)>) -> Occupancy {
        let mut occupancy = Occupancy::default();
        for (footprint, owner) in claims {
            occupancy.claim(footprint, owner);
        }
        occupancy
    }

    /// Record a claim.
    pub fn claim(&mut self, footprint: Footprint, owner: impl Into<String>) {
        let slots = self.universes.entry(footprint.universe).or_default();
        let at = slots.partition_point(|(f, _)| f.first < footprint.first);
        slots.insert(at, (footprint, owner.into()));
    }

    /// Drop every claim owned by `owner`, so a fixture can be re-addressed
    /// without colliding with where it already is.
    pub fn release(&mut self, owner: &str) {
        for slots in self.universes.values_mut() {
            slots.retain(|(_, held)| held != owner);
        }
    }

    /// Who already holds a channel `footprint` wants, if anyone.
    #[must_use]
    pub fn conflict(&self, footprint: &Footprint) -> Option<&str> {
        self.universes
            .get(&footprint.universe)?
            .iter()
            .find(|(held, _)| held.overlaps(footprint))
            .map(|(_, owner)| owner.as_str())
    }

    /// The lowest address in `universe`, at or after `from`, where `channels`
    /// consecutive free channels start.
    #[must_use]
    pub fn first_free(&self, universe: u16, from: u16, channels: u16) -> Option<u16> {
        let mut candidate = from.max(1);
        if let Some(slots) = self.universes.get(&universe) {
            for (held, _) in slots {
                if held.last() < candidate {
                    continue;
                }
                if candidate + channels - 1 < held.first {
                    break;
                }
                candidate = held.last() + 1;
            }
        }
        Footprint::new(universe, candidate, channels).map(|f| f.address())
    }

    /// Every claim in one universe as 512 cells, `cells[i]` being address
    /// `i + 1`.
    ///
    /// The one query a footprint strip needs: a cell names its fixture, which
    /// channel of it this is, and whether more than one fixture claims it.
    #[must_use]
    pub fn cells(&self, universe: u16) -> Vec<Cell> {
        let mut cells = vec![Cell::default(); UNIVERSE_SIZE as usize];
        let Some(slots) = self.universes.get(&universe) else {
            return cells;
        };
        for (held, owner) in slots {
            for address in held.first..=held.last() {
                let cell = &mut cells[address as usize - 1];
                if cell.fixture.is_some() {
                    cell.collision = true;
                } else {
                    cell.fixture = Some(owner.clone());
                    cell.channel = address - held.first;
                }
            }
        }
        cells
    }

    /// Every universe that anything is patched into, ascending.
    pub fn universes(&self) -> impl Iterator<Item = u16> + '_ {
        self.universes.keys().copied()
    }
}

/// One DMX channel of one universe.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cell {
    /// The fixture holding this channel, or `None` if it is free.
    pub fixture: Option<String>,
    /// Which channel of that fixture this is, zero-based.
    pub channel: u16,
    /// Another fixture also claims it. Only reachable from rows written before
    /// anything validated — nothing can create one now.
    pub collision: bool,
}

// ---------------------------------------------------------------------------
// The rule
// ---------------------------------------------------------------------------

/// Where a fixture hangs, as the allocator needs it: which run, and how far
/// along.
///
/// `along` is metres along the run's own span axis — the truss family is
/// authored with its span on local `+X` (`luma_render::truss`, "Local space"),
/// so a fixture's position in its run's frame *is* `along(t)` in metres, for a
/// run of one segment or of four.
#[derive(Clone, Debug, PartialEq)]
struct OnRun {
    run: String,
    along: f64,
}

/// The run a fixture hangs from, and how far along it sits.
///
/// The **nearest** ancestor of kind `run` or `tower`: a fixture bolted to a
/// piece — a clamp, a bracket, a corner block — bolted to a run is on that run.
/// A fixture that reaches the venue root without passing one is not on a run at
/// all.
///
/// # The limit: a run is one straight length
///
/// "Nearest" is doing real work here, and it is not the same as "the structure
/// this fixture is part of". `venue_graph::kind_of` maps *every* truss to
/// [`NodeKind::Run`], so a straight–corner–straight assembly is two runs, not
/// one, and it takes two universes. That is deliberate rather than a rounding
/// error: `along` is `local.w_axis.x`, the fixture's position on its run's own
/// span axis, and past a corner that axis turns — a single `along` over the
/// whole assembly would not be monotone, so its "physical order" would be a
/// lie. One universe per straight run keeps the order true.
///
/// A compound run — several lengths a human names as one structure, addressed
/// as one universe — is B4's job (`docs/specs/venue-builder-gauntlet.md` §6),
/// and needs a run *node* over the pieces rather than a walk that guesses.
/// `straight_corner_straight_is_two_runs_and_two_blocks` pins today's answer.
fn on_run(venue: &ResolvedVenue, fixture: &str) -> Option<OnRun> {
    let pose = venue.pose(fixture)?;
    let mut ancestor = pose.parent.as_deref();
    while let Some(id) = ancestor {
        let node = venue.pose(id)?;
        if matches!(node.kind, NodeKind::Run | NodeKind::Tower) {
            let local = node.world.inverse() * pose.world;
            return Some(OnRun {
                run: id.to_string(),
                along: local.w_axis.x,
            });
        }
        ancestor = node.parent.as_deref();
    }
    None
}

/// Assign every fixture a universe and an address.
///
/// Total and deterministic: same venue and same fixture list in, byte-identical
/// assignments out. A fixture the venue does not place still gets an address —
/// it is in the tray, not lost — and the only fixture that gets none is one
/// there was genuinely no room for, which is reported as [`Note::NoRoom`].
#[must_use]
pub fn allocate(venue: &ResolvedVenue, fixtures: &[Fixture]) -> Allocation {
    let mut allocation = Allocation::default();
    let mut occupancy = Occupancy::default();

    // Pins first, so every derived block flows around them rather than through
    // them. They are emitted in the input's order and keep their place in the
    // output by fixture id, not by where the run walk would have put them.
    for fixture in fixtures.iter().filter(|f| f.address.pin().is_some()) {
        let footprint = fixture.address.pin().expect("filtered");
        occupancy.claim(footprint, fixture.id.clone());
        allocation.assignments.push(Assignment {
            fixture: fixture.id.clone(),
            footprint,
            run: on_run(venue, &fixture.id).map(|p| p.run),
            pinned: true,
        });
    }

    // Runs in solve order — depth-first from the root — so universe 1 is the
    // first structure in the rig rather than whichever id sorts first.
    let mut runs: Vec<String> = Vec::new();
    let mut members: BTreeMap<String, Vec<(&Fixture, f64)>> = BTreeMap::new();
    let mut loose: Vec<&Fixture> = Vec::new();
    for fixture in fixtures.iter().filter(|f| f.address.pin().is_none()) {
        match on_run(venue, &fixture.id) {
            Some(OnRun { run, along }) => {
                if !members.contains_key(&run) {
                    runs.push(run.clone());
                }
                members.entry(run).or_default().push((fixture, along));
            }
            None => loose.push(fixture),
        }
    }
    runs.sort_by_key(|run| {
        venue
            .poses()
            .position(|p| p.node == *run)
            .unwrap_or(usize::MAX)
    });

    let mut next_universe = 1u16;
    for run in runs {
        let mut on_this_run = members.remove(&run).unwrap_or_default();
        // Physical order along the run; ties by node id so two fixtures at the
        // same station still order the same way every solve.
        on_this_run.sort_by(|(a, along_a), (b, along_b)| {
            along_a
                .partial_cmp(along_b)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.id.cmp(&b.id))
        });
        let block: u32 = on_this_run.iter().map(|(f, _)| u32::from(f.channels)).sum();

        // The whole block in one universe, or — only if the run is wider than a
        // universe can hold — split across as few as it takes.
        let placed = u16::try_from(block)
            .ok()
            .filter(|block| *block <= UNIVERSE_SIZE)
            .and_then(|block| {
                (next_universe..MAX_UNIVERSE)
                    .find_map(|u| occupancy.first_free(u, 1, block).map(|start| (u, start)))
            });

        match placed {
            Some((universe, start)) => {
                if universe != next_universe {
                    allocation.notes.push(Note::RunRolled {
                        run: run.clone(),
                        offered: next_universe,
                        taken: universe,
                    });
                }
                let mut address = start;
                for (fixture, _) in on_this_run {
                    let footprint = Footprint::new(universe, address, fixture.channels)
                        .expect("the block was checked to fit before it was walked");
                    occupancy.claim(footprint, fixture.id.clone());
                    allocation.assignments.push(Assignment {
                        fixture: fixture.id.clone(),
                        footprint,
                        run: Some(run.clone()),
                        pinned: false,
                    });
                    address += fixture.channels;
                }
                next_universe = universe.saturating_add(1);
            }
            None => {
                let mut universes = Vec::new();
                for (fixture, _) in on_this_run {
                    match place(&mut occupancy, next_universe, fixture) {
                        Some(footprint) => {
                            next_universe = footprint.universe();
                            if universes.last() != Some(&next_universe) {
                                universes.push(next_universe);
                            }
                            allocation.assignments.push(Assignment {
                                fixture: fixture.id.clone(),
                                footprint,
                                run: Some(run.clone()),
                                pinned: false,
                            });
                        }
                        None => allocation.notes.push(Note::NoRoom {
                            fixture: fixture.id.clone(),
                        }),
                    }
                }
                allocation.notes.push(Note::RunSplit {
                    run: run.clone(),
                    universes,
                });
                next_universe = next_universe.saturating_add(1);
            }
        }
    }

    // The tray, and anything resting on the floor: no run, so no universe of
    // its own — it fills the gaps the run blocks left, from universe 1.
    loose.sort_by(|a, b| a.id.cmp(&b.id));
    for fixture in loose {
        match place(&mut occupancy, 1, fixture) {
            Some(footprint) => allocation.assignments.push(Assignment {
                fixture: fixture.id.clone(),
                footprint,
                run: None,
                pinned: false,
            }),
            None => allocation.notes.push(Note::NoRoom {
                fixture: fixture.id.clone(),
            }),
        }
    }

    allocation
}

/// The first free slot at or after `universe`, rolling on until one is found.
fn place(occupancy: &mut Occupancy, universe: u16, fixture: &Fixture) -> Option<Footprint> {
    let mut candidate = universe;
    while candidate < MAX_UNIVERSE {
        if let Some(address) = occupancy.first_free(candidate, 1, fixture.channels) {
            let footprint = Footprint::new(candidate, address, fixture.channels)?;
            occupancy.claim(footprint, fixture.id.clone());
            return Some(footprint);
        }
        candidate += 1;
    }
    None
}

/// Where the next `count` fixtures of `channels` channels each would go if they
/// were added to `run` now.
///
/// What a distribution needs *before* its fixtures exist: it has to write patch
/// rows in the same transaction that writes the placement, and until the
/// placement exists [`allocate`] has nothing to order them by. The addresses
/// are appended at the end of the run's block, which is the answer for the
/// common case of distributing onto an empty truss; a distribution that
/// interleaves with fixtures already on the run is put in physical order by the
/// next [`allocate`], which is what auto-patch is for.
///
/// `run` of `None` asks for tray addresses — the run-less rule, from universe 1.
///
/// # Which occupancy this answers against
///
/// **Both**, and that is the whole subtlety. A caller writes the offered
/// address into a row, and that write is admitted against the *stored* patch
/// (`services::patch::admit`), which is where the fixtures are **now** — not
/// where [`allocate`] would put them. The two pictures diverge the moment
/// anything is added without an auto-patch after it, so an offer that consults
/// only the derived allocation hands out a slot some stored row already holds
/// and the write is refused.
///
/// So a slot is offered only if it is free in the union: free where the rows
/// are, *and* free where the rule would put them. Free-in-union implies
/// free-in-stored, so the write cannot be refused; and it implies the offer
/// does not sit in a run block the next auto-patch will claim, so the number
/// the human just read does not move under them for no reason.
#[must_use]
pub fn next_addresses(
    venue: &ResolvedVenue,
    fixtures: &[Fixture],
    run: Option<&str>,
    channels: u16,
    count: usize,
) -> Vec<Footprint> {
    let existing = allocate(venue, fixtures);
    let mut occupancy = Occupancy::of(
        existing
            .assignments
            .iter()
            .map(|a| (a.footprint, a.fixture.clone()))
            .chain(
                fixtures
                    .iter()
                    .filter_map(|f| Some((f.address.footprint()?, f.id.clone()))),
            ),
    );

    let highest = |of_run: Option<&str>| {
        existing
            .assignments
            .iter()
            .filter(|a| of_run.is_none() || a.run.as_deref() == of_run)
            .map(|a| a.footprint.universe())
            .max()
    };
    let start = match run {
        // The tray fills gaps from universe 1, same as the run-less rule.
        None => 1,
        // A run that already carries fixtures continues in its own universe;
        // the occupancy above is what pushes these past its block.
        Some(run) => match highest(Some(run)) {
            Some(universe) => universe,
            // A run with nothing on it yet has no universe, so it takes the
            // first one nothing else has claimed.
            None => highest(None).unwrap_or(0).saturating_add(1),
        },
    };

    let mut out = Vec::with_capacity(count);
    for index in 0..count {
        let placeholder = Fixture {
            id: format!("\u{0}pending:{index}"),
            channels,
            address: Address::Unset,
        };
        match place(&mut occupancy, start, &placeholder) {
            Some(footprint) => out.push(footprint),
            None => break,
        }
    }
    out
}

#[cfg(test)]
mod tests;
