//! What every control on the Perform surface shows, decided in one place.
//!
//! Luma decides what each control looks like; the screen and every
//! controller draw that as well as they can. [`VenueControls::surface`]
//! answers, for each control of every page, its job and how it stands now,
//! from the bank and the layout it was installed with.
//!
//! A controller shows a look as a [`Light`] ([`Look::light`]): an effect
//! dim while off, bright while on, blinking while it waits for its beat and
//! pulsing while Select has it, in the color it plays (a hit in amber); a
//! group lit while chosen or held; a value in its color, bright while on; a
//! utility lit while on, and tap on each beat.

use std::collections::BTreeMap;
use std::time::Instant;

use luma_patterns::clip_graph::LiveInputKind;
use luma_patterns::press::PressMode;
use luma_patterns::BlendMode;
use serde::Serialize;

use super::perform::{self, PartKind};
use super::{Bank, VenueControls, ALL, UNSET_COLOR};
use crate::models::perform::{CellContent, ListValue, PerformLayout, PerformPattern};

/// Every control on the surface, by name, and how it looks.
pub type Surface = BTreeMap<String, Look>;

/// What a control does: the five jobs of the Perform surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Job {
    /// A pattern's pad, or another button of its graph.
    Effect,
    /// A venue group to choose.
    Group,
    /// A list's value, or its Next.
    Value,
    /// A fader: a pattern's level or fader node, a fader cell, the master.
    Fader,
    /// Select, Clear or tap.
    Utility,
}

/// How one control stands.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Look {
    pub job: Job,
    /// An effect or button on, a group chosen, a value on (a Next: any of
    /// its list's), Select on, tap in the first part of a beat.
    pub on: bool,
    /// A press waits for its beat or bar.
    pub waiting: bool,
    /// A group, Select or Clear held down.
    pub held: bool,
    /// An effect chosen by Select.
    pub selected: bool,
    /// The color it shows, linear Rec. 2020 rgb at full brightness: an
    /// effect's the color it plays (its own, or the base color where it
    /// plays when it takes the color below; amber for a hit), a value's its
    /// own, a group's its base color. White where there is none.
    pub color: [f64; 3],
    /// A fader's position, 0–1.
    pub value: f64,
    /// Tap's beat of the bar, from 0.
    pub beat: Option<u8>,
}

/// How a light shows: held, breathing, or flashing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Shine {
    Steady,
    Pulse,
    Blink,
}

/// A look as a controller's light shows it: a color, bright or dim, and
/// how it shines.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Light {
    pub color: [f64; 3],
    pub bright: bool,
    pub shine: Shine,
}

/// What a hit (a pad that flashes) shows.
pub const AMBER: [f64; 3] = [1.0, 0.45, 0.0];

/// Tap is on for this much of each beat.
const TAP_ON: f64 = 0.25;

impl Look {
    /// The light this look is on a controller.
    pub fn light(&self) -> Light {
        let steady = |bright| Light {
            color: self.color,
            bright,
            shine: Shine::Steady,
        };
        match self.job {
            Job::Effect if self.waiting => Light {
                shine: Shine::Blink,
                ..steady(true)
            },
            Job::Effect if self.selected => Light {
                shine: Shine::Pulse,
                ..steady(true)
            },
            Job::Effect | Job::Value => steady(self.on),
            Job::Group => steady(self.on || self.held),
            Job::Fader => steady(self.value > 0.),
            Job::Utility => Light {
                color: UNSET_COLOR,
                ..steady(self.on || self.held)
            },
        }
    }
}

/// What a control is, from the layout; the bank says how it stands.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Face {
    Pad {
        pattern: String,
        hit: bool,
    },
    Button {
        pattern: String,
    },
    Fader,
    Group {
        group: String,
    },
    /// Value `index` of list `list`, or with `None` its Next.
    Value {
        list: String,
        index: Option<usize>,
    },
    Tap,
    Select,
    Clear,
}

/// Each control of every page of `layout`, of a set of `patterns`, and
/// what it is. A live color is set by a list, not played, so it has none.
pub(super) fn faces(layout: &PerformLayout, patterns: &[PerformPattern]) -> BTreeMap<String, Face> {
    let mut faces = BTreeMap::new();
    for cell in layout.pages.iter().flat_map(|page| &page.cells) {
        for part in perform::parts(cell, patterns) {
            let pattern = || cell.pattern_id().unwrap_or_default().to_owned();
            let face = match part.kind {
                PartKind::Pad => {
                    let hit = patterns
                        .iter()
                        .find(|one| Some(one.id.as_str()) == cell.pattern_id())
                        .and_then(|one| one.graph.pad())
                        .is_some_and(|pad| {
                            matches!(pad.kind, LiveInputKind::Button(press) if press.mode == PressMode::Flash)
                        });
                    Face::Pad {
                        pattern: pattern(),
                        hit,
                    }
                }
                PartKind::Button => Face::Button { pattern: pattern() },
                PartKind::Fader { .. } => Face::Fader,
                PartKind::Color => continue,
                PartKind::Value { index } => Face::Value {
                    list: cell.id.clone(),
                    index: Some(index),
                },
                PartKind::Next => Face::Value {
                    list: cell.id.clone(),
                    index: None,
                },
                PartKind::Group => match &cell.content {
                    CellContent::Group { group, .. } => Face::Group {
                        group: group.clone(),
                    },
                    _ => continue,
                },
                PartKind::Tap => Face::Tap,
                PartKind::Select => Face::Select,
                PartKind::Clear => Face::Clear,
            };
            faces.insert(part.control, face);
        }
    }
    faces
}

impl Bank {
    /// Every control on the surface and how it looks at `at`.
    pub(super) fn surface(&self, at: Instant) -> Surface {
        self.faces
            .iter()
            .map(|(name, face)| (name.clone(), self.look(name, face, at)))
            .collect()
    }

    /// How control `name`, which is `face`, stands at `at`.
    fn look(&self, name: &str, face: &Face, at: Instant) -> Look {
        let look = |job, color| Look {
            job,
            on: false,
            waiting: false,
            held: false,
            selected: false,
            color,
            value: 0.,
            beat: None,
        };
        let button = || self.buttons.get(name).map(|held| held.at(at));
        let pressed = |job, color| {
            let button = button();
            Look {
                on: button.is_some_and(|button| button.on()),
                waiting: button.is_some_and(|button| button.waiting()),
                ..look(job, color)
            }
        };
        match face {
            Face::Pad { pattern, hit } => {
                let color = if *hit { AMBER } else { self.plays(pattern, at) };
                Look {
                    selected: self.selected.contains(pattern),
                    ..pressed(Job::Effect, color)
                }
            }
            Face::Button { pattern } => pressed(Job::Effect, self.plays(pattern, at)),
            Face::Fader => Look {
                value: self.value(name, at),
                ..look(Job::Fader, UNSET_COLOR)
            },
            Face::Group { group } => Look {
                on: self.chosen.contains(group),
                held: self
                    .held
                    .keys()
                    .any(|control| self.groups.get(control) == Some(group)),
                ..look(Job::Group, self.base_of(group, at))
            },
            Face::Value { list, index } => {
                let list = self.lists.get(list);
                let on = list.and_then(|list| list.on);
                let shown = index.or(on).unwrap_or(0);
                let color = match list.and_then(|list| list.values.get(shown)) {
                    Some(ListValue::Color(rgb)) => *rgb,
                    _ => UNSET_COLOR,
                };
                Look {
                    on: match index {
                        Some(index) => on == Some(*index),
                        None => on.is_some(),
                    },
                    ..look(Job::Value, color)
                }
            }
            Face::Tap => {
                let (beat, beats_per_bar) = self.beat(at);
                Look {
                    on: beat.rem_euclid(1.) < TAP_ON,
                    beat: Some(beat.rem_euclid(beats_per_bar).floor() as u8),
                    ..look(Job::Utility, UNSET_COLOR)
                }
            }
            Face::Select => Look {
                on: self.select.on(),
                held: self.select.down.is_some(),
                ..look(Job::Utility, UNSET_COLOR)
            },
            Face::Clear => Look {
                held: self.clearing.is_some(),
                ..look(Job::Utility, UNSET_COLOR)
            },
        }
    }

    /// The color pattern `id` plays at `at`: the base color where it plays
    /// when it takes the color below, else its own live color, else white.
    fn plays(&self, id: &str, at: Instant) -> [f64; 3] {
        let Some(effect) = self.effects.get(id) else {
            return UNSET_COLOR;
        };
        if effect.blend != BlendMode::Below {
            return effect
                .color
                .as_ref()
                .map_or(UNSET_COLOR, |color| self.color(color, at));
        }
        let groups: Vec<&str> = match effect.groups.as_slice() {
            [] if self.lights.contains_key(&effect.selection) => vec![effect.selection.as_str()],
            [] => vec![ALL],
            groups => groups.iter().map(String::as_str).collect(),
        };
        let lights: std::collections::BTreeSet<&String> = groups
            .into_iter()
            .filter_map(|group| self.lights.get(group))
            .flat_map(|lights| lights.iter())
            .collect();
        self.mean_base(lights, at)
    }

    /// Group `group`'s base color at `at`: the mean of its lights'.
    pub(super) fn base_of(&self, group: &str, at: Instant) -> [f64; 3] {
        self.mean_base(
            self.lights
                .get(group)
                .into_iter()
                .flat_map(|lights| lights.iter()),
            at,
        )
    }

    /// The mean base color of `lights` at `at`, each white while it has
    /// none; white for no lights.
    fn mean_base<'a>(&self, lights: impl IntoIterator<Item = &'a String>, at: Instant) -> [f64; 3] {
        let (mut sum, mut count) = ([0.; 3], 0.);
        for light in lights {
            let rgb = self.base_at(light, at);
            (0..3).for_each(|c| sum[c] += rgb[c]);
            count += 1.;
        }
        if count == 0. {
            UNSET_COLOR
        } else {
            sum.map(|c| c / count)
        }
    }
}

impl VenueControls {
    /// Every control on the surface and how it looks at `at`.
    pub(crate) fn surface_at(&self, at: Instant) -> Surface {
        self.read().surface(at)
    }
}
