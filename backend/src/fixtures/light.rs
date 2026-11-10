//! What a library fixture's light is: how a request for each cell becomes the
//! values its channels take ([`ModeLight::realize`]), and what light those
//! values make ([`ModeLight::emit`]). The DMX engine encodes what `realize`
//! decides and the stage draws what `emit` makes of it, so the picture is the
//! rig's light and not the request.
//!
//! A cell emits through a `colour/<emitter>` channel only where the channel
//! covers it and the cell has a die the channel lights
//! ([`luma_fixture::EmitterSet::lit_by`]). A channel shared by several cells
//! carries the mean of what the cells that can emit through it ask for. A
//! dimmer the fixture lacks is made by scaling the emitters, and a strobe no
//! shutter can make is made by gating the cell's brightness on the strobe
//! clock.

use std::sync::Arc;

use luma_fixture::control::{Attr, Param, Resolved};
use luma_fixture::{Built, CellEmitters, EmitterColour, EmitterSet};
use luma_patterns::color_space::Gamut;
use luma_scene::strobe::Slice;

use crate::models::universe::PrimitiveState;

/// Where an emitter's light sits, as linear sRGB at its full level. Fixture
/// definitions do not say, so these are assumptions, like the sRGB primaries
/// assumed for red, green and blue: a D65 white, a 590 nm amber and a
/// phosphor lime (CIE xy 0.40, 0.55), each clipped into sRGB.
pub(crate) const WHITE_EMITTER: [f64; 3] = [1.0, 1.0, 1.0];
pub(crate) const AMBER_EMITTER: [f64; 3] = [1.0, 0.2, 0.0];
pub(crate) const LIME_EMITTER: [f64; 3] = [0.66, 1.0, 0.0];

/// Two strobe values closer than one step of an 8-bit shutter are the same
/// value on the wire, so one shutter can carry both.
const STROBE_TOLERANCE: f32 = 1.0 / 256.0;

/// The light an emitter of `colour` makes at full level, linear sRGB. The
/// mixer makes colours only from red, green, blue, the whites, amber and
/// lime; the rest show what they give off when a dimmer lights them: UV the
/// faint violet a UV LED shows to the eye.
fn emitter_light(colour: EmitterColour) -> [f64; 3] {
    match colour {
        EmitterColour::Red => [1.0, 0.0, 0.0],
        EmitterColour::Green => [0.0, 1.0, 0.0],
        EmitterColour::Blue => [0.0, 0.0, 1.0],
        EmitterColour::White | EmitterColour::WarmWhite | EmitterColour::CoolWhite => WHITE_EMITTER,
        EmitterColour::Amber => AMBER_EMITTER,
        EmitterColour::Lime => LIME_EMITTER,
        EmitterColour::Cyan => [0.0, 1.0, 1.0],
        EmitterColour::Indigo => [0.25, 0.0, 1.0],
        EmitterColour::Mint => [0.4, 1.0, 0.6],
        EmitterColour::Orange => [1.0, 0.45, 0.0],
        EmitterColour::Uv => [0.08, 0.0, 0.2],
    }
}

fn is_white(colour: EmitterColour) -> bool {
    matches!(
        colour,
        EmitterColour::White | EmitterColour::WarmWhite | EmitterColour::CoolWhite
    )
}

/// One colour slot of a wheel or preset channel: the byte that selects it and
/// its colour, gamma-encoded sRGB as the spec writes it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Slot {
    pub byte: u8,
    pub rgb: [f32; 3],
}

/// What one param of the mode does to the light.
#[derive(Debug, Clone, PartialEq)]
enum Role {
    /// An additive emitter; `cells` are the covered cells it lights.
    Emitter {
        colour: EmitterColour,
        cells: Vec<usize>,
    },
    Dimmer {
        cells: Vec<usize>,
    },
    /// One CMY flag (0 cyan, 1 magenta, 2 yellow) over a white source.
    Filter {
        primary: usize,
        cells: Vec<usize>,
    },
    Shutter {
        cells: Vec<usize>,
    },
    /// A colour wheel or preset with colours to pick from.
    Wheel {
        slots: Vec<Slot>,
        cells: Vec<usize>,
    },
    /// Not part of the light: position, speed, beam, gobos, ...
    Other,
}

/// One cell of the mode: which emitters reach it and how its colour is made.
#[derive(Debug, Clone, PartialEq)]
struct Cell {
    /// The emitter channels that light it, after the geometry has had its
    /// say.
    mix: EmitterSet,
    /// Its colour comes from CMY flags over a white lamp.
    subtractive: bool,
    /// Its colour comes from a wheel: a dark colour can only dim the lamp.
    wheel: bool,
    /// No channel sets its colour, but a dimmer sets its brightness: it
    /// shines in its dies' colour (white where the geometry does not say).
    lamp: Option<[f64; 3]>,
}

/// A library fixture's mode, resolved for its light. Built once per
/// definition and mode.
#[derive(Debug, Clone, PartialEq)]
pub struct ModeLight {
    cells: Vec<Cell>,
    /// One per resolved param, in the mode's order. A copy (below) is
    /// [`Role::Other`] here.
    roles: Vec<Role>,
    /// `(copy, original)`: a param with the same attribute over the same
    /// cells as an earlier one (`n: 1` and `n: 2` of one colour, two
    /// dimmers). A copy is a second control of the same light, so it takes
    /// its original's value and adds nothing to the light.
    copies: Vec<(usize, usize)>,
}

/// What `realize` decided for one param.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Value {
    /// A level, 0–1 of the param's full width: an emitter, dimmer or flag.
    Level(f32),
    /// A shutter strobing at this strobe value (0–1).
    Strobe(f32),
    /// A shutter held open.
    Open,
    /// A wheel or preset on this slot.
    Slot(u8),
    /// A wheel left where it is: nothing asks for a colour.
    Hold,
    /// Not part of the light; the engine drives it as before.
    Other,
}

/// A mode's realized light for one moment.
#[derive(Debug, Clone, PartialEq)]
pub struct Realized {
    /// One per resolved param.
    pub values: Vec<Value>,
    /// Per cell, the strobe value its fixture's own shutter flashes it at;
    /// 0 when the shutter is open.
    pub own_strobe: Vec<f32>,
}

/// One cell's light: linear sRGB at its brightness.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CellLight {
    pub rgb: [f32; 3],
    /// The strobe value its fixture's own shutter flashes it at; 0 when it
    /// does not.
    pub own_strobe: f32,
}

impl ModeLight {
    /// The mode `resolved`, whose params as the engine reads them are
    /// `params`, on `built`. `slots` gives a wheel or preset param's colour
    /// slots (empty for any other).
    #[must_use]
    pub fn new(
        built: &Built,
        resolved: &Resolved,
        params: &[Arc<Param>],
        slots: impl Fn(&Param) -> Vec<Slot>,
    ) -> Self {
        let n = built.cells.len().max(1);
        let driven = resolved.driven_emitters(n);
        // A cell's dies: what its geometry draws, plus any emitter a channel
        // scoped to it drives that the geometry leaves out (the library test
        // holds the shipped specs to none), plus any colour an emulated
        // channel makes from the dies it has. A cell the geometry cannot
        // speak for has whatever drives it.
        let mut dies: Vec<EmitterSet> = (0..n)
            .map(|c| match built.cell_emitters(c) {
                CellEmitters::Known(set) => set,
                CellEmitters::Unknown | CellEmitters::Invalid => driven[c],
            })
            .collect();
        for p in params.iter().filter(|p| p.cell.is_some() || p.emulated) {
            if let Some(e) = EmitterColour::of(p.attr) {
                for c in covered(p, n) {
                    if !dies[c].lit_by(e) {
                        dies[c].insert(e);
                    }
                }
            }
        }
        let mix: Vec<EmitterSet> = (0..n)
            .map(|c| driven[c].iter().filter(|&e| dies[c].lit_by(e)).collect())
            .collect();
        let mut roles: Vec<Role> = params
            .iter()
            .map(|p| {
                let cells = covered(p, n);
                match p.attr {
                    a if EmitterColour::of(a).is_some() => {
                        let colour = EmitterColour::of(a).expect("matched");
                        let lit: Vec<usize> = cells
                            .iter()
                            .copied()
                            .filter(|&c| mix[c].contains(colour))
                            .collect();
                        Role::Emitter { colour, cells: lit }
                    }
                    Attr::Dimmer => Role::Dimmer { cells },
                    Attr::CmyCyan => Role::Filter { primary: 0, cells },
                    Attr::CmyMagenta => Role::Filter { primary: 1, cells },
                    Attr::CmyYellow => Role::Filter { primary: 2, cells },
                    Attr::Shutter => Role::Shutter { cells },
                    Attr::ColourWheel | Attr::ColourMacro => {
                        let slots = slots(p);
                        if slots.is_empty() {
                            Role::Other
                        } else {
                            Role::Wheel { slots, cells }
                        }
                    }
                    _ => Role::Other,
                }
            })
            .collect();
        let mut copies = Vec::new();
        for (i, p) in params.iter().enumerate() {
            let original = params[..i].iter().position(|q| {
                q.attr == p.attr && q.wheel == p.wheel && covered(q, n) == covered(p, n)
            });
            if let Some(j) = original.filter(|_| !matches!(roles[i], Role::Other)) {
                copies.push((i, j));
                roles[i] = Role::Other;
            }
        }
        // Where channels of one colour overlap (a master colour over pixel
        // colours), each cell is the narrowest one's: a wider one carries
        // only the cells no narrower one reaches.
        let spans: Vec<Option<(EmitterColour, usize)>> = roles
            .iter()
            .map(|r| match r {
                Role::Emitter { colour, cells } => Some((*colour, cells.len())),
                _ => None,
            })
            .collect();
        let owner = |colour: EmitterColour, c: usize| -> Option<usize> {
            roles
                .iter()
                .enumerate()
                .filter(|(_, r)| matches!(r, Role::Emitter { colour: e, cells } if *e == colour && cells.contains(&c)))
                .min_by_key(|(i, _)| spans[*i].map_or(usize::MAX, |(_, w)| w))
                .map(|(i, _)| i)
        };
        let owned: Vec<Vec<usize>> = roles
            .iter()
            .enumerate()
            .map(|(i, r)| match r {
                Role::Emitter { colour, cells } => cells
                    .iter()
                    .copied()
                    .filter(|&c| owner(*colour, c) == Some(i))
                    .collect(),
                _ => Vec::new(),
            })
            .collect();
        for (role, own) in roles.iter_mut().zip(owned) {
            if let Role::Emitter { cells, .. } = role {
                *cells = own;
            }
        }
        let cells = (0..n)
            .map(|c| {
                let additive = !mix[c].is_empty();
                let covers = |want: fn(&Role) -> Option<&Vec<usize>>| {
                    roles
                        .iter()
                        .filter_map(want)
                        .any(|cells| cells.contains(&c))
                };
                let subtractive = !additive
                    && covers(|r| match r {
                        Role::Filter { cells, .. } => Some(cells),
                        _ => None,
                    });
                let wheel = !additive
                    && !subtractive
                    && covers(|r| match r {
                        Role::Wheel { cells, .. } => Some(cells),
                        _ => None,
                    });
                let dimmed = covers(|r| match r {
                    Role::Dimmer { cells } => Some(cells),
                    _ => None,
                });
                let lamp = (!additive && !subtractive && !wheel && dimmed).then(|| lamp(dies[c]));
                Cell {
                    mix: mix[c],
                    subtractive,
                    wheel,
                    lamp,
                }
            })
            .collect();
        Self {
            cells,
            roles,
            copies,
        }
    }

    /// How many cells the fixture has (at least one).
    #[must_use]
    pub fn cells(&self) -> usize {
        self.cells.len()
    }

    /// The values that make `requests` (one per cell, in the working colour
    /// space) as nearly as the fixture can, over `slice` of the strobe clock.
    /// `max_dimmer` is the grand master. The DMX engine passes a slice of no
    /// length: the gate at its open.
    #[must_use]
    pub fn realize(&self, requests: &[PrimitiveState], max_dimmer: f32, slice: Slice) -> Realized {
        let n = self.cells.len();
        let request = |c: usize| requests.get(c).or_else(|| requests.first());
        let max_dimmer = f64::from(max_dimmer.clamp(0.0, 1.0));

        // Each cell's colour (emitter levels at a peak of 1) and brightness.
        let mut levels = vec![[0.0_f64; 3]; n];
        let mut brightness = vec![0.0_f64; n];
        let mut strobe = vec![0.0_f32; n];
        for c in 0..n {
            let Some(p) = request(c) else { continue };
            let (l, b) = Gamut::SRGB.split(p.color.map(f64::from), f64::from(p.dimmer));
            levels[c] = l;
            brightness[c] = b * max_dimmer;
            if self.cells[c].wheel {
                // A wheel cannot dim its colour, so a dark colour dims the lamp.
                brightness[c] *= f64::from(luma(l.map(|v| v as f32)));
            }
            strobe[c] = if p.strobe.is_finite() {
                p.strobe.max(0.0)
            } else {
                0.0
            };
        }

        let mut values = vec![Value::Other; self.roles.len()];

        // Strobe: a shutter flashes the cells it covers when they all ask
        // for the same rate, narrowest shutters first; any other strobing
        // cell is gated on the clock.
        let mut own_strobe = vec![0.0_f32; n];
        let mut shutters: Vec<(usize, &Vec<usize>)> = self
            .roles
            .iter()
            .enumerate()
            .filter_map(|(i, r)| match r {
                Role::Shutter { cells } => Some((i, cells)),
                _ => None,
            })
            .collect();
        shutters.sort_by_key(|(_, cells)| cells.len());
        for (i, cells) in shutters {
            let first = cells.first().map_or(0.0, |&c| strobe[c]);
            let agree = cells
                .iter()
                .all(|&c| (strobe[c] - first).abs() < STROBE_TOLERANCE && own_strobe[c] == 0.0);
            values[i] = if agree && first > 0.0 {
                for &c in cells {
                    own_strobe[c] = first;
                }
                Value::Strobe(first)
            } else {
                Value::Open
            };
        }
        for c in 0..n {
            if strobe[c] > 0.0 && own_strobe[c] == 0.0 {
                brightness[c] *= f64::from(luma_scene::strobe::gain(strobe[c], slice));
            }
        }

        // Brightness goes to the gains over each cell, widest first: every
        // dimmer, and every set of cells that share emitter channels. Each is
        // set by the brightest of what is left of its cells' brightness, and
        // the narrower ones make up the rest; on a tie a dimmer goes first,
        // so a dimmer dims and the colour stays at full.
        let mut residual = brightness;
        let mut gains: Vec<(&Vec<usize>, Option<usize>)> = Vec::new();
        for (i, role) in self.roles.iter().enumerate() {
            match role {
                Role::Dimmer { cells } => gains.push((cells, Some(i))),
                Role::Emitter { cells, .. }
                    if !cells.is_empty()
                        && !gains.iter().any(|(g, d)| d.is_none() && *g == cells) =>
                {
                    gains.push((cells, None));
                }
                _ => {}
            }
        }
        gains.sort_by_key(|(cells, dimmer)| (std::cmp::Reverse(cells.len()), dimmer.is_none()));
        let mut shared: Vec<(&Vec<usize>, f64)> = Vec::new();
        for (cells, dimmer) in gains {
            let level = cells.iter().map(|&c| residual[c]).fold(0.0, f64::max);
            if level > 0.0 {
                for &c in cells {
                    residual[c] = (residual[c] / level).min(1.0);
                }
            }
            match dimmer {
                Some(i) => values[i] = Value::Level(level as f32),
                None => shared.push((cells, level)),
            }
        }

        // Each emitter channel: the mean of its cells' mix of that colour, at
        // its cells' shared gain.
        let mixes: Vec<Mix> = (0..n)
            .map(|c| Mix::of(levels[c], self.cells[c].mix))
            .collect();
        for (i, role) in self.roles.iter().enumerate() {
            match role {
                Role::Emitter { colour, cells } => {
                    let gain = shared
                        .iter()
                        .find(|(g, _)| *g == cells)
                        .map_or(0.0, |(_, l)| *l);
                    values[i] = Value::Level(
                        (f64::from(mean(cells.iter().map(|&c| mixes[c].level(*colour)))) * gain)
                            as f32,
                    );
                }
                Role::Filter { primary, cells } => {
                    // A flag passes 1 − its level of the white lamp; with no
                    // dimmer to darken the lamp, the flags close toward black.
                    values[i] = Value::Level(mean(cells.iter().map(|&c| {
                        if self.cells[c].subtractive {
                            1.0 - levels[c][*primary] * residual[c]
                        } else {
                            0.0
                        }
                    })));
                }
                Role::Wheel { slots, cells } => {
                    let Some(&c) = cells.first() else { continue };
                    if self.cells[c].subtractive {
                        // The flags make the colour; the wheel stays out of
                        // the beam on its whitest slot.
                        values[i] = nearest_slot(slots, [1.0; 3])
                            .map_or(Value::Hold, |s| Value::Slot(s.byte));
                        continue;
                    }
                    let srgb = levels[c].map(|v| v as f32);
                    values[i] = if srgb.iter().all(|&v| v <= 1e-4) {
                        Value::Hold
                    } else {
                        nearest_slot(slots, srgb).map_or(Value::Hold, |s| Value::Slot(s.byte))
                    };
                }
                Role::Dimmer { .. } | Role::Shutter { .. } | Role::Other => {}
            }
        }
        for &(copy, original) in &self.copies {
            values[copy] = values[original];
        }
        Realized { values, own_strobe }
    }

    /// The light each cell gives at `realized`'s values: the sum of the
    /// emitters that reach it, through every dimmer over it, or its lamp
    /// through its flags or wheel. A wheel on [`Value::Hold`] shows white.
    #[must_use]
    pub fn emit(&self, realized: &Realized) -> Vec<CellLight> {
        let n = self.cells.len();
        let level = |i: usize| match realized.values.get(i) {
            Some(Value::Level(v)) => f64::from(*v),
            _ => 0.0,
        };
        let mut dim = vec![1.0_f64; n];
        // Two channels driving one emitter on a cell (a master colour over
        // pixel colours) do not add: the cell's emitter shows the higher.
        let mut emitters = vec![[0.0_f64; EmitterColour::ALL.len()]; n];
        let mut flags = vec![[0.0_f64; 3]; n];
        let mut wheel = vec![[1.0_f64; 3]; n];
        for (i, role) in self.roles.iter().enumerate() {
            match role {
                Role::Dimmer { cells } => cells.iter().for_each(|&c| dim[c] *= level(i)),
                Role::Emitter { colour, cells } => {
                    for &c in cells {
                        let e = &mut emitters[c][*colour as usize];
                        *e = e.max(level(i));
                    }
                }
                Role::Filter { primary, cells } => {
                    cells.iter().for_each(|&c| flags[c][*primary] = level(i));
                }
                Role::Wheel { slots, cells } => {
                    if let Some(Value::Slot(byte)) = realized.values.get(i) {
                        if let Some(s) = slots.iter().find(|s| s.byte == *byte) {
                            let linear = s.rgb.map(decode);
                            cells.iter().for_each(|&c| wheel[c] = linear);
                        }
                    }
                }
                Role::Shutter { .. } | Role::Other => {}
            }
        }
        (0..n)
            .map(|c| {
                let cell = &self.cells[c];
                // A cell's white channels (white, warm, cool) all carry its
                // white, as a console sends them, and light as one white.
                let white = [
                    EmitterColour::White,
                    EmitterColour::WarmWhite,
                    EmitterColour::CoolWhite,
                ]
                .iter()
                .map(|&e| emitters[c][e as usize])
                .fold(0.0, f64::max);
                let rgb = EmitterColour::ALL.iter().filter(|&&e| !is_white(e)).fold(
                    WHITE_EMITTER.map(|v| v * white),
                    |sum, &e| {
                        let light = emitter_light(e);
                        let level = emitters[c][e as usize];
                        std::array::from_fn(|k| sum[k] + level * light[k])
                    },
                );
                let colour = if let Some(lamp) = cell.lamp {
                    lamp
                } else if cell.subtractive {
                    flags[c].map(|f| 1.0 - f)
                } else if cell.wheel {
                    wheel[c]
                } else {
                    rgb.map(|v| v.min(1.0))
                };
                CellLight {
                    rgb: colour.map(|v| (v * dim[c]) as f32),
                    own_strobe: realized.own_strobe.get(c).copied().unwrap_or(0.0),
                }
            })
            .collect()
    }
}

/// The colour a cell with `dies` shines in when no channel sets it: white if
/// its dies make white (red, green and blue, or a white) or the geometry
/// names none, else its dies' light together at a peak of 1 (a UV bar's
/// violet, an amber's amber).
/// A die set the stage shows no light for is dark.
fn lamp(dies: EmitterSet) -> [f64; 3] {
    use EmitterColour::{Blue, Green, Red};
    let mixes_white = (dies.contains(Red) && dies.contains(Green) && dies.contains(Blue))
        || dies.iter().any(is_white);
    if dies.is_empty() || mixes_white {
        // A dimmer-only mode of a fixture that can make white is its
        // calibrated white.
        return WHITE_EMITTER;
    }
    let sum = dies.iter().fold([0.0; 3], |sum, e| {
        let light = emitter_light(e);
        std::array::from_fn(|k| sum[k] + light[k])
    });
    let peak = sum.iter().copied().fold(0.0, f64::max);
    if peak > 0.0 {
        sum.map(|v| v / peak)
    } else {
        [0.0; 3]
    }
}

/// The cells `p` covers on a fixture of `n`: its span, or all of them.
fn covered(p: &Param, n: usize) -> Vec<usize> {
    match p.cell {
        Some(cell) => {
            let [a, b] = cell.span();
            (a as usize..=(b as usize).min(n - 1)).collect()
        }
        None => (0..n).collect(),
    }
}

fn mean(values: impl Iterator<Item = f64>) -> f32 {
    let (sum, count) = values.fold((0.0, 0_u32), |(s, k), v| (s + v, k + 1));
    if count == 0 {
        0.0
    } else {
        (sum / f64::from(count)) as f32
    }
}

/// `#rrggbb` as gamma-encoded sRGB, 0–1.
pub(crate) fn hex(s: &str) -> Option<[f32; 3]> {
    let hex = s.trim().strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
    Some([byte(0)?, byte(2)?, byte(4)?].map(|b| f32::from(b) / 255.0))
}

/// A gamma-encoded sRGB component as linear light.
fn decode(v: f32) -> f64 {
    let v = f64::from(v.clamp(0.0, 1.0));
    if v <= 0.040_45 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// Rec. 601 luma of a gamma-encoded colour (0–1).
pub(crate) fn luma(rgb: [f32; 3]) -> f32 {
    0.299 * rgb[0] + 0.587 * rgb[1] + 0.114 * rgb[2]
}

/// One cell's emitter levels for a colour.
#[derive(Debug, Clone, Copy, Default)]
struct Mix {
    red: f64,
    green: f64,
    blue: f64,
    /// Every white channel that reaches the cell carries it.
    white: f64,
    amber: f64,
    lime: f64,
}

impl Mix {
    /// `levels` (linear sRGB at a peak of 1) as the emitters in `mix` make
    /// it. Each extra emitter takes as much of the colour as it can make, and
    /// red, green and blue make what is left; a colour the cell has no
    /// emitter for is lost.
    fn of(levels: [f64; 3], mix: EmitterSet) -> Self {
        let mut rest = levels;
        let mut take = |present: bool, emitter: [f64; 3]| {
            if !present {
                return 0.0;
            }
            let share = (0..3)
                .filter(|&i| emitter[i] > 0.0)
                .map(|i| rest[i] / emitter[i])
                .fold(1.0, f64::min)
                .max(0.0);
            for i in 0..3 {
                rest[i] = (rest[i] - share * emitter[i]).max(0.0);
            }
            share
        };
        let white = take(mix.iter().any(is_white), WHITE_EMITTER);
        let amber = take(mix.contains(EmitterColour::Amber), AMBER_EMITTER);
        let lime = take(mix.contains(EmitterColour::Lime), LIME_EMITTER);
        let has = |e| if mix.contains(e) { 1.0 } else { 0.0 };
        Self {
            red: rest[0] * has(EmitterColour::Red),
            green: rest[1] * has(EmitterColour::Green),
            blue: rest[2] * has(EmitterColour::Blue),
            white,
            amber,
            lime,
        }
    }

    fn level(&self, colour: EmitterColour) -> f64 {
        match colour {
            EmitterColour::Red => self.red,
            EmitterColour::Green => self.green,
            EmitterColour::Blue => self.blue,
            EmitterColour::White | EmitterColour::WarmWhite | EmitterColour::CoolWhite => {
                self.white
            }
            EmitterColour::Amber => self.amber,
            EmitterColour::Lime => self.lime,
            EmitterColour::Cyan
            | EmitterColour::Indigo
            | EmitterColour::Mint
            | EmitterColour::Orange
            | EmitterColour::Uv => 0.0,
        }
    }
}

/// The slot nearest `desired` (gamma-encoded sRGB), weighing saturation so a
/// grey picks open white rather than a saturated slot that happens to be
/// close.
pub(crate) fn nearest_slot(slots: &[Slot], desired: [f32; 3]) -> Option<Slot> {
    slots
        .iter()
        .copied()
        .min_by(|a, b| distance(a.rgb, desired).total_cmp(&distance(b.rgb, desired)))
}

fn distance(slot: [f32; 3], desired: [f32; 3]) -> f32 {
    fn saturation(rgb: [f32; 3]) -> f32 {
        let max = rgb[0].max(rgb[1]).max(rgb[2]);
        let min = rgb[0].min(rgb[1]).min(rgb[2]);
        if max < 1e-4 {
            0.0
        } else {
            (max - min) / max
        }
    }
    let rgb: f32 = (0..3).map(|i| (slot[i] - desired[i]).powi(2)).sum();
    let (want, have) = (saturation(desired), saturation(slot));
    let penalty = if want < 0.3 {
        have * have * 2.0
    } else {
        (want - have).abs() * 0.5
    };
    rgb + penalty
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::library::{fixture_path, index};

    fn lit(dimmer: f32, srgb: [f64; 3]) -> PrimitiveState {
        let working = luma_patterns::color_space::from_linear_srgb(srgb);
        PrimitiveState {
            dimmer,
            color: working.map(|v| v as f32),
            strobe: 0.0,
            position: [0.0, 0.0],
            speed: 0.0,
            aim: None,
        }
    }

    const STILL: Slice = Slice {
        open: 0.0,
        length: 0.0,
    };

    /// Every shipped fixture's every mode, with its light.
    fn every_mode() -> Vec<(String, String, Arc<ModeLight>)> {
        let fixtures = crate::resources::source_fixtures_root().expect("the shipped bundle");
        let library = crate::resources::fixture_library_root(Some(&fixtures)).unwrap();
        let mut out = Vec::new();
        for entry in &index(&library).unwrap().fixtures {
            let def = crate::fixtures::definition(&fixtures, &fixture_path(&entry.id)).unwrap();
            let source = def.library.as_ref().unwrap();
            for (mode, light) in &source.light {
                out.push((entry.id.clone(), mode.clone(), light.clone()));
            }
        }
        assert!(!out.is_empty());
        out
    }

    fn light_of(light: &ModeLight, request: &PrimitiveState) -> Vec<CellLight> {
        let requests = vec![request.clone(); light.cells()];
        light.emit(&light.realize(&requests, 1.0, STILL))
    }

    /// A cell with red, green and blue makes any colour inside sRGB, at any
    /// brightness, when every cell asks for the same and the channels it
    /// shares reach only cells like it: what comes out is what was asked for.
    #[test]
    fn an_rgb_cell_gives_the_light_asked_for() {
        use EmitterColour::{Blue, Green, Red};
        let colours = [
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 1.0],
            [1.0, 0.5, 0.25],
            [0.2, 0.9, 0.6],
        ];
        let mut failures = Vec::new();
        for (id, mode, light) in every_mode() {
            for colour in colours {
                for dimmer in [1.0_f32, 0.4] {
                    let out = light_of(&light, &lit(dimmer, colour));
                    for (c, cell) in light.cells.iter().enumerate() {
                        // A channel shared with unlike cells carries a mean
                        // neither gets exactly.
                        let alike = light.roles.iter().all(|r| match r {
                            Role::Emitter { cells, .. } if cells.contains(&c) => {
                                cells.iter().all(|&o| light.cells[o].mix == cell.mix)
                            }
                            _ => true,
                        });
                        if !(alike
                            && cell.mix.contains(Red)
                            && cell.mix.contains(Green)
                            && cell.mix.contains(Blue))
                        {
                            continue;
                        }
                        let want = colour.map(|v| v * f64::from(dimmer));
                        let got = out[c].rgb;
                        if (0..3).any(|k| (f64::from(got[k]) - want[k]).abs() > 0.01) {
                            failures
                                .push(format!("{id} {mode} cell {c}: asked {want:?}, got {got:?}"));
                        }
                    }
                }
            }
        }
        assert!(
            failures.is_empty(),
            "{}\n{}",
            failures.len(),
            failures[..failures.len().min(20)].join("\n")
        );
    }

    /// Dimmer 0 is dark wherever the fixture can make dark: through a dimmer
    /// over the cell, or its emitters.
    #[test]
    fn dimmer_zero_is_dark() {
        let mut failures = Vec::new();
        for (id, mode, light) in every_mode() {
            let out = light_of(&light, &lit(0.0, [1.0, 0.6, 0.3]));
            for (c, cell) in light.cells.iter().enumerate() {
                let dimmed = light
                    .roles
                    .iter()
                    .any(|r| matches!(r, Role::Dimmer { cells } if cells.contains(&c)));
                if (dimmed || !cell.mix.is_empty()) && out[c].rgb.iter().any(|&v| v > 1e-4) {
                    failures.push(format!("{id} {mode} cell {c}: {:?}", out[c].rgb));
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    /// More dimmer is never less light.
    #[test]
    fn light_rises_with_dimmer() {
        let mut failures = Vec::new();
        for (id, mode, light) in every_mode() {
            let mut before = light_of(&light, &lit(0.0, [0.8, 0.5, 1.0]));
            for step in 1..=10 {
                let now = light_of(&light, &lit(step as f32 / 10.0, [0.8, 0.5, 1.0]));
                for c in 0..light.cells() {
                    if (0..3).any(|k| now[c].rgb[k] + 1e-5 < before[c].rgb[k]) {
                        failures.push(format!(
                            "{id} {mode} cell {c} at {step}: {:?} < {:?}",
                            now[c].rgb, before[c].rgb
                        ));
                    }
                }
                before = now;
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    /// Cells asking for different colours on a channel they share get the
    /// same light: they cannot be told apart on the wire.
    #[test]
    fn cells_on_the_same_channels_get_the_same_light() {
        let mut failures = Vec::new();
        for (id, mode, light) in every_mode() {
            let n = light.cells();
            if n < 2 {
                continue;
            }
            let requests: Vec<PrimitiveState> = (0..n)
                .map(|c| {
                    lit(
                        1.0,
                        if c % 2 == 0 {
                            [1.0, 0.2, 0.0]
                        } else {
                            [0.0, 0.3, 1.0]
                        },
                    )
                })
                .collect();
            let out = light.emit(&light.realize(&requests, 1.0, STILL));
            // Cells covered by exactly the same params.
            let key = |c: usize| -> Vec<usize> {
                light
                    .roles
                    .iter()
                    .enumerate()
                    .filter(|(_, r)| match r {
                        Role::Emitter { cells, .. }
                        | Role::Dimmer { cells }
                        | Role::Filter { cells, .. }
                        | Role::Wheel { cells, .. } => cells.contains(&c),
                        Role::Shutter { .. } | Role::Other => false,
                    })
                    .map(|(i, _)| i)
                    .collect()
            };
            for a in 0..n {
                for b in a + 1..n {
                    if key(a) == key(b)
                        && light.cells[a] == light.cells[b]
                        && out[a].rgb != out[b].rgb
                    {
                        failures.push(format!(
                            "{id} {mode} cells {a},{b}: {:?} vs {:?}",
                            out[a].rgb, out[b].rgb
                        ));
                    }
                }
            }
        }
        assert!(
            failures.is_empty(),
            "{}",
            failures[..failures.len().min(20)].join("\n")
        );
    }
}
