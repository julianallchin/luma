use crate::fixtures::light::{
    hex, luma, nearest_slot, ModeLight, Slot, Value, AMBER_EMITTER, LIME_EMITTER, WHITE_EMITTER,
};
use crate::models::fixtures::{
    Channel, ChannelColour, ChannelType, FixtureDefinition, Mode, PatchedFixture,
};
use crate::models::universe::{PrimitiveState, UniverseState};
use luma_fixture::control::Attr;
use luma_patterns::color_space::Gamut;
use std::collections::HashMap;

/// Tolerance for "heads agree" — matches the master shutter channel's DMX resolution.
/// Two strobe values within 1/256 of each other can't be distinguished on the wire,
/// so they're treated as the same value for cascade decisions.
const HEADS_AGREE_TOLERANCE: f32 = 1.0 / 256.0;

/// Square-wave gate frequency: `hz = strobe * STROBE_HZ_MAX`. The visualizer uses
/// the same maximum so on-screen and physical strobe stay phase-coherent.
const STROBE_HZ_MAX: f64 = 20.0;

/// Cascade rung chosen for the fixture this frame. The cascade prefers, in order:
/// per-head shutter → master shutter (if heads agree) → per-head dimmer pulse →
/// master dimmer pulse (if heads agree) → per-head color pulse → master color pulse
/// (if heads agree) → silent drop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StrobeRung {
    None,
    PerHeadShutter,
    MasterShutter,
    PerHeadDimmer,
    MasterDimmer,
    PerHeadColor,
    MasterColor,
}

/// What to write to the fixture's master shutter channel (if any) this frame.
#[derive(Debug, Clone, Copy)]
enum MasterShutterAction {
    /// No master shutter channel exists on this fixture.
    NotApplicable,
    /// Write the strobe capability at this unified strobe value (cascade chose master shutter).
    UseStrobe(f32),
    /// Cascade chose a different rung; neutralize the master shutter so it doesn't
    /// chop our software gating. Writes the Open capability, or Hold if none found.
    WriteOpen,
}

/// Per-fixture strobe decisions, computed once at the top of the fixture loop and
/// consumed by `map_value` / `apply_strobe_*` helpers.
struct FixtureStrobeCtx {
    rung: StrobeRung,
    /// Per-head gate state under per-head rungs. `true` = ON phase (no gating);
    /// `false` = OFF phase, force the head's gated channels to 0.
    /// Heads with strobe <= 0 are always `true`.
    head_gate_open: Vec<bool>,
    /// Single gate state under master rungs (computed from the unified strobe value).
    master_gate_open: bool,
    /// What the master shutter channel should write this frame.
    master_shutter_action: MasterShutterAction,
    /// Strobe value to write to each head's per-head shutter channel under
    /// `PerHeadShutter` rung. Unused otherwise.
    head_strobes: Vec<f32>,
}

fn strobe_gate_open(strobe: f32, t: f64) -> bool {
    if strobe <= 0.0 {
        return true;
    }
    let hz = (strobe as f64) * STROBE_HZ_MAX;
    if hz <= 0.0 {
        return true;
    }
    let period = 1.0 / hz;
    (t.rem_euclid(period)) <= period * 0.5
}

fn heads_agree(values: &[f32]) -> bool {
    if values.len() <= 1 {
        return true;
    }
    let (mn, mx) = values
        .iter()
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(a, b), &v| {
            (a.min(v), b.max(v))
        });
    (mx - mn) < HEADS_AGREE_TOLERANCE
}

/// Inventory of strobe-capable actuators on a fixture's mode, plus per-head
/// presence flags so the resolver can answer "does every strobing head have a
/// dimmer channel of its own?"
struct StrobeInventory {
    head_count: usize,
    has_master_shutter: bool,
    has_master_dimmer: bool,
    has_master_color: bool,
    per_head_shutter: Vec<bool>,
    per_head_dimmer: Vec<bool>,
    per_head_color: Vec<bool>,
}

fn build_strobe_inventory(def: &FixtureDefinition, mode: &Mode) -> StrobeInventory {
    let head_count = mode.heads.len();
    // Channels grouped by their head_idx (None = master).
    let mut head_of: HashMap<u32, usize> = HashMap::new();
    for (h_idx, head) in mode.heads.iter().enumerate() {
        for &ch_num in &head.channels {
            head_of.insert(ch_num, h_idx);
        }
    }

    let mut has_master_shutter = false;
    let mut has_master_dimmer = false;
    let mut has_master_color = false;
    let mut per_head_shutter = vec![false; head_count];
    let mut per_head_dimmer = vec![false; head_count];
    let mut per_head_color = vec![false; head_count];

    for mode_ch in &mode.channels {
        let Some(ch) = def.channels.iter().find(|c| c.name == mode_ch.name) else {
            continue;
        };
        let ch_type = ch.get_type();
        let ch_colour = ch.get_colour();
        let head_idx = head_of.get(&mode_ch.number).copied();
        let is_dimmer = ch_type == ChannelType::Intensity && ch_colour == ChannelColour::None;
        let is_color_intensity =
            ch_type == ChannelType::Intensity && ch_colour != ChannelColour::None;
        let is_shutter = ch_type == ChannelType::Shutter;

        match head_idx {
            None => {
                if is_shutter {
                    has_master_shutter = true;
                }
                if is_dimmer {
                    has_master_dimmer = true;
                }
                if is_color_intensity {
                    has_master_color = true;
                }
            }
            Some(h) => {
                if is_shutter {
                    per_head_shutter[h] = true;
                }
                if is_dimmer {
                    per_head_dimmer[h] = true;
                }
                if is_color_intensity {
                    per_head_color[h] = true;
                }
            }
        }
    }

    StrobeInventory {
        head_count,
        has_master_shutter,
        has_master_dimmer,
        has_master_color,
        per_head_shutter,
        per_head_dimmer,
        per_head_color,
    }
}

/// Whether a fixture in `mode_name` strobes on its own clock: its mode has a
/// shutter channel, so the engine writes a strobe rate there and the fixture
/// does the flashing, rather than the engine pulsing a dimmer or a color. The
/// stage view simulates that clock for these fixtures so it flashes as the rig
/// does. An unknown mode is `false`.
#[must_use]
pub fn strobes_on_own_clock(def: &FixtureDefinition, mode_name: &str) -> bool {
    def.modes
        .iter()
        .find(|m| m.name == mode_name)
        .is_some_and(|mode| {
            let inv = build_strobe_inventory(def, mode);
            inv.has_master_shutter || inv.per_head_shutter.iter().any(|&h| h)
        })
}

fn resolve_strobe_rung(inv: &StrobeInventory, head_strobes: &[f32], agree: bool) -> StrobeRung {
    let strobing: Vec<usize> = head_strobes
        .iter()
        .enumerate()
        .filter(|(_, &s)| s > 0.0)
        .map(|(i, _)| i)
        .collect();
    if strobing.is_empty() {
        return StrobeRung::None;
    }

    // Rung 1: every strobing head has its own shutter channel.
    if !inv.per_head_shutter.is_empty()
        && strobing
            .iter()
            .all(|&h| inv.per_head_shutter.get(h).copied().unwrap_or(false))
    {
        return StrobeRung::PerHeadShutter;
    }
    // Rung 2: master shutter + heads agree.
    if inv.has_master_shutter && agree {
        return StrobeRung::MasterShutter;
    }
    // Rung 3: every strobing head has its own dimmer.
    if !inv.per_head_dimmer.is_empty()
        && strobing
            .iter()
            .all(|&h| inv.per_head_dimmer.get(h).copied().unwrap_or(false))
    {
        return StrobeRung::PerHeadDimmer;
    }
    // Rung 4: master dimmer + heads agree.
    if inv.has_master_dimmer && agree {
        return StrobeRung::MasterDimmer;
    }
    // Rung 5: every strobing head has color intensity channels.
    if !inv.per_head_color.is_empty()
        && strobing
            .iter()
            .all(|&h| inv.per_head_color.get(h).copied().unwrap_or(false))
    {
        return StrobeRung::PerHeadColor;
    }
    // Rung 6: master color + heads agree.
    if inv.has_master_color && agree {
        return StrobeRung::MasterColor;
    }
    StrobeRung::None
}

fn build_strobe_ctx(
    state: &UniverseState,
    fixture: &PatchedFixture,
    def: &FixtureDefinition,
    mode: &Mode,
    frame_time_secs: f64,
) -> FixtureStrobeCtx {
    let inv = build_strobe_inventory(def, mode);

    // Resolve per-head strobe values. For fixtures with declared heads, each head
    // gets its own primitive's strobe (falling back to the fixture-level primitive
    // if no head primitive is registered). For headless fixtures, use the fixture
    // primitive directly so the master rungs see a single-element "agree" set.
    let fixture_strobe = state.primitives.get(&fixture.id).map(|p| p.strobe);
    let head_count = inv.head_count.max(1);
    let head_strobes: Vec<f32> = (0..head_count)
        .map(|h| {
            state
                .primitives
                .get(&format!("{}:{}", fixture.id, h))
                .map(|p| p.strobe)
                .or(fixture_strobe)
                .unwrap_or(0.0)
        })
        .collect();

    // For agreement, include both the per-head values and the fixture-level value
    // (if it exists) — a fixture primitive disagreeing with heads is still a "no
    // single master value works" signal.
    let mut agree_set: Vec<f32> = head_strobes.clone();
    if let Some(fs) = fixture_strobe {
        agree_set.push(fs);
    }
    let agree = heads_agree(&agree_set);

    let rung = resolve_strobe_rung(&inv, &head_strobes, agree);

    // Unified strobe value when heads agree — used for master rungs.
    let unified_strobe = if agree {
        head_strobes.iter().copied().fold(0.0_f32, f32::max)
    } else {
        0.0
    };

    let head_gate_open: Vec<bool> = head_strobes
        .iter()
        .map(|&s| strobe_gate_open(s, frame_time_secs))
        .collect();
    let master_gate_open = strobe_gate_open(unified_strobe, frame_time_secs);

    let master_shutter_action = if !inv.has_master_shutter {
        MasterShutterAction::NotApplicable
    } else if rung == StrobeRung::MasterShutter {
        MasterShutterAction::UseStrobe(unified_strobe)
    } else {
        MasterShutterAction::WriteOpen
    };

    FixtureStrobeCtx {
        rung,
        head_gate_open,
        master_gate_open,
        master_shutter_action,
        head_strobes,
    }
}

/// Capability-driven shutter write — handles both "open the shutter" and "strobe at
/// rate v" using whatever the channel's capabilities advertise. When the cascade
/// needs to neutralize a master shutter (per-head fallback selected) this is the
/// path that writes the Open / LampOn capability instead of blindly writing 0.
fn write_open_capability(channel: &Channel) -> MapAction {
    if let Some(lowered) = &channel.lowered {
        return MapAction::Set(lowered.open());
    }
    if let Some(cap) = channel.capabilities.iter().find(|c| {
        let preset = c.preset.as_deref().unwrap_or("");
        let label = c.label.to_lowercase();
        preset.contains("Open") || preset.contains("LampOn") || label.contains("open")
    }) {
        return MapAction::Set(cap.min);
    }
    // Without an Open capability we can't safely assume 0 means "shutter open"
    // (some fixtures use 0 = closed). Hold the previous frame's value instead.
    MapAction::Hold
}

fn write_strobe_capability(channel: &Channel, strobe: f32) -> MapAction {
    let strobe = strobe.clamp(0.0, 1.0);
    if let Some(lowered) = &channel.lowered {
        return MapAction::Set(lowered.strobe(strobe, f64::from(strobe) * STROBE_HZ_MAX));
    }
    if let Some(cap) = channel.capabilities.iter().find(|c| c.is_strobe()) {
        let range = (cap.max - cap.min) as f32;
        let val = cap.min as f32 + (strobe * range);
        return MapAction::Set(val.clamp(cap.min as f32, cap.max as f32) as u8);
    }
    // Generic strobe channel without capabilities — linear 10..255 mapping.
    MapAction::Set(((strobe * 245.0) + 10.0) as u8)
}

fn shutter_action(channel: &Channel, head_idx: Option<usize>, ctx: &FixtureStrobeCtx) -> MapAction {
    match head_idx {
        // Per-head shutter channel
        Some(h) => {
            let strobe = ctx.head_strobes.get(h).copied().unwrap_or(0.0);
            if ctx.rung == StrobeRung::PerHeadShutter && strobe > 0.0 {
                write_strobe_capability(channel, strobe)
            } else {
                write_open_capability(channel)
            }
        }
        // Master shutter channel
        None => match ctx.master_shutter_action {
            MasterShutterAction::UseStrobe(v) => write_strobe_capability(channel, v),
            MasterShutterAction::WriteOpen | MasterShutterAction::NotApplicable => {
                write_open_capability(channel)
            }
        },
    }
}

fn apply_intensity_gate(
    mapped: MapAction,
    channel: &Channel,
    head_idx: Option<usize>,
    ctx: &FixtureStrobeCtx,
) -> MapAction {
    let MapAction::Set(_) = mapped else {
        return mapped;
    };
    // Only Intensity-group channels carry brightness; Pan/Tilt/Speed/Gobo/etc.
    // must pass through untouched even during strobe gating so movement and
    // beam shape don't chop.
    if channel.get_type() != ChannelType::Intensity {
        return mapped;
    }
    let colour = channel.get_colour();
    let is_dimmer = colour == ChannelColour::None;
    let is_color = !is_dimmer;

    let should_gate = match (ctx.rung, head_idx) {
        (StrobeRung::PerHeadDimmer, Some(h)) if is_dimmer => {
            !ctx.head_gate_open.get(h).copied().unwrap_or(true)
        }
        (StrobeRung::MasterDimmer, None) if is_dimmer => !ctx.master_gate_open,
        (StrobeRung::PerHeadColor, Some(h)) if is_color => {
            !ctx.head_gate_open.get(h).copied().unwrap_or(true)
        }
        (StrobeRung::MasterColor, None) if is_color => !ctx.master_gate_open,
        _ => false,
    };

    if should_gate {
        MapAction::Set(0)
    } else {
        mapped
    }
}

pub fn generate_dmx(
    state: &UniverseState,
    fixtures: &[PatchedFixture],
    definitions: &HashMap<String, FixtureDefinition>,
    previous_universe_buffers: Option<&HashMap<i64, [u8; 512]>>,
    max_dimmer: f32,
    frame_time_secs: f64,
) -> HashMap<i64, [u8; 512]> {
    let mut buffers: HashMap<i64, [u8; 512]> = HashMap::new();
    let max_dimmer = max_dimmer.clamp(0.0, 1.0);

    for fixture in fixtures {
        let def = match definitions.get(&fixture.fixture_path) {
            Some(d) => d,
            None => continue,
        };

        let mode = match def.modes.iter().find(|m| m.name == fixture.mode_name) {
            Some(m) => m,
            None => continue,
        };

        if let Some(light) = def
            .library
            .as_ref()
            .and_then(|source| source.light.get(&mode.name))
        {
            let buffer = buffers.entry(fixture.universe).or_insert([0; 512]);
            let prev = previous_universe_buffers.and_then(|m| m.get(&fixture.universe));
            write_library(
                state,
                fixture,
                def,
                mode,
                light,
                max_dimmer,
                frame_time_secs,
                buffer,
                prev,
            );
            continue;
        }

        let has_master_dimmer = mode.channels.iter().any(|mode_channel| {
            let channel = match def.channels.iter().find(|c| c.name == mode_channel.name) {
                Some(c) => c,
                None => return false,
            };
            channel.get_type() == ChannelType::Intensity
                && channel.get_colour() == ChannelColour::None
        });

        let has_color_wheel = def.has_color_wheel(mode);
        let emitters = Emitters::of(def, mode);
        // Each head's color in channel terms, worked out once per head.
        let mut colors: Vec<(*const PrimitiveState, HeadColor)> = Vec::new();

        let [pan_max, tilt_max] = def.travel().map(|[lo, hi]| hi - lo);

        let buffer = buffers.entry(fixture.universe).or_insert([0; 512]);
        let prev = previous_universe_buffers.and_then(|m| m.get(&fixture.universe));

        // Map channel index to head index
        let mut channel_to_head: HashMap<u32, usize> = HashMap::new();
        for (head_idx, head) in mode.heads.iter().enumerate() {
            for &channel_idx in &head.channels {
                channel_to_head.insert(channel_idx, head_idx);
            }
        }

        let strobe_ctx = build_strobe_ctx(state, fixture, def, mode, frame_time_secs);

        for mode_channel in &mode.channels {
            let channel_number = mode_channel.number as usize;
            let dmx_address = (fixture.address - 1) as usize + channel_number;
            // Unreachable for an address reason: a patch row's footprint is
            // checked into existence by `luma_scene::patch::Footprint` on the
            // way in and again by the `fixtures_address_fits_universe_*`
            // triggers, so `address + num_channels - 1 <= 512` holds for every
            // row in the table. What can still exceed it is a *mode* whose
            // channel numbers run past the `num_channels` the row was patched
            // with — a definition/patch mismatch, not an address — and the
            // assertion is where that would surface instead of going dark
            // silently.
            debug_assert!(
                dmx_address < 512,
                "{} channel {channel_number} lands at {dmx_address}: mode {} declares more \
                 channels than the patch row's {}",
                fixture.id,
                fixture.mode_name,
                fixture.num_channels
            );
            if dmx_address >= 512 {
                continue;
            }

            // Find the channel definition
            let channel = match def.channels.iter().find(|c| c.name == mode_channel.name) {
                Some(c) => c,
                None => continue,
            };

            // Determine which Primitive ID to use (Head vs Fixture)
            let fixture_prim = state.primitives.get(&fixture.id);
            let head0_prim = state.primitives.get(&format!("{}:0", fixture.id));
            let head_idx = channel_to_head.get(&mode_channel.number).copied();
            let head_prim = head_idx.and_then(|h_idx| {
                let head_id = format!("{}:{}", fixture.id, h_idx);
                state.primitives.get(&head_id)
            });

            // If a dimmer channel ends up in a <Head>, it still usually represents a
            // fixture-level master dimmer. Prefer fixture primitive dimmer in that case.
            let prim = match (head_prim, fixture_prim, head0_prim) {
                // Most specific: exact head primitive exists
                (Some(h), Some(f), _) => {
                    let ch_type = channel.get_type();
                    let ch_colour = channel.get_colour();
                    if ch_type == ChannelType::Intensity && ch_colour == ChannelColour::None {
                        f
                    } else {
                        h
                    }
                }
                (Some(h), None, _) => h,
                // No head mapping (or missing head primitive): use fixture primitive if present
                (None, Some(f), _) => f,
                // Fallback: selection system often targets "fixture:0" even for single-head fixtures
                (None, None, Some(h0)) => h0,
                (None, None, None) => continue,
            };

            // Shutter channels are owned by the strobe cascade — it knows whether
            // this fixture is using its shutter (and which value) or neutralizing it
            // because a lower rung is doing the strobe.
            let mapped = if channel.get_type() == ChannelType::Shutter {
                shutter_action(channel, head_idx, &strobe_ctx)
            } else {
                let color = match colors.iter().find(|(p, _)| std::ptr::eq(*p, prim)) {
                    Some((_, color)) => color,
                    None => {
                        let color = HeadColor::new(prim, emitters, has_color_wheel);
                        colors.push((prim, color));
                        &colors.last().expect("just pushed").1
                    }
                };
                let m = map_value(
                    channel,
                    prim,
                    color,
                    pan_max,
                    tilt_max,
                    max_dimmer,
                    has_master_dimmer,
                );
                apply_intensity_gate(m, channel, head_idx, &strobe_ctx)
            };

            match mapped {
                MapAction::Set(v) => buffer[dmx_address] = v,
                MapAction::Hold => {
                    if let Some(prev_buf) = prev {
                        buffer[dmx_address] = prev_buf[dmx_address];
                    }
                }
            }
        }
    }

    buffers
}

/// A library fixture's channels, from what its light realizes for the
/// cells' primitives: each cell reads its own primitive, else the fixture's,
/// else cell 0's.
#[allow(clippy::too_many_arguments)]
fn write_library(
    state: &UniverseState,
    fixture: &PatchedFixture,
    def: &FixtureDefinition,
    mode: &Mode,
    light: &ModeLight,
    max_dimmer: f32,
    frame_time_secs: f64,
    buffer: &mut [u8; 512],
    prev: Option<&[u8; 512]>,
) {
    let requests = requests(state, &fixture.id, light.cells());
    if requests.is_empty() {
        return;
    }
    let slice = luma_scene::strobe::Slice {
        open: frame_time_secs,
        length: 0.0,
    };
    let realized = light.realize(&requests, max_dimmer, slice);
    let position = requests[0].position;
    for mode_channel in &mode.channels {
        let dmx_address = (fixture.address - 1) as usize + mode_channel.number as usize;
        debug_assert!(
            dmx_address < 512,
            "{} channel {} lands at {dmx_address}: mode {} declares more channels than the \
             patch row's {}",
            fixture.id,
            mode_channel.number,
            fixture.mode_name,
            fixture.num_channels
        );
        if dmx_address >= 512 {
            continue;
        }
        let Some(lowered) = def
            .channels
            .iter()
            .find(|c| c.name == mode_channel.name)
            .and_then(|c| c.lowered.as_deref())
        else {
            continue;
        };
        let p = &lowered.param;
        let byte = match realized.values.get(lowered.index).copied() {
            Some(Value::Level(v)) => Some(p.byte(
                (f64::from(v.clamp(0.0, 1.0)) * f64::from(p.max())).round() as u32,
                lowered.rank,
            )),
            Some(Value::Strobe(s)) => Some(lowered.strobe(s, f64::from(s) * STROBE_HZ_MAX)),
            Some(Value::Open) => Some(lowered.open()),
            Some(Value::Slot(b)) => Some(if lowered.rank == 0 { b } else { lowered.home() }),
            Some(Value::Hold) => None,
            Some(Value::Other) | None => {
                if !lowered.driven() {
                    Some(lowered.home())
                } else if p.attr.is_position() {
                    let axis = usize::from(p.attr == Attr::Tilt);
                    (!position[axis].is_nan()).then(|| lowered.position(position[axis]))
                } else {
                    Some(lowered.home())
                }
            }
        };
        match byte {
            Some(v) => buffer[dmx_address] = v,
            None => {
                if let Some(prev) = prev {
                    buffer[dmx_address] = prev[dmx_address];
                }
            }
        }
    }
}

/// Each of a library fixture's `cells` cells' primitive: its own, else the
/// fixture's, else cell 0's. Empty when the universe has none of them.
pub(crate) fn requests(state: &UniverseState, id: &str, cells: usize) -> Vec<PrimitiveState> {
    let fixture = state.primitives.get(id);
    let first = state.primitives.get(&format!("{id}:0"));
    if cells <= 1 {
        return fixture.or(first).into_iter().cloned().collect();
    }
    let own: Vec<Option<&PrimitiveState>> = (0..cells)
        .map(|c| state.primitives.get(&format!("{id}:{c}")))
        .collect();
    if own.iter().all(Option::is_none) && fixture.is_none() {
        return Vec::new();
    }
    own.into_iter()
        .map(|p| p.or(fixture).or(first).cloned().unwrap_or(DARK))
        .collect()
}

/// A cell nothing lights.
const DARK: PrimitiveState = PrimitiveState {
    dimmer: 0.0,
    color: [0.0; 3],
    strobe: 0.0,
    position: [f32::NAN, f32::NAN],
    speed: 0.0,
    aim: None,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MapAction {
    Set(u8),
    Hold,
}

#[allow(clippy::too_many_arguments)]
fn map_value(
    channel: &crate::models::fixtures::Channel,
    state: &PrimitiveState,
    color: &HeadColor,
    pan_max_deg: f32,
    tilt_max_deg: f32,
    max_dimmer: f32,
    has_master_dimmer: bool,
) -> MapAction {
    let ch_type = channel.get_type();
    let emitter = |colour| {
        MapAction::Set(scale_u8(
            level_u8(color.level(colour)),
            max_dimmer,
            !has_master_dimmer,
        ))
    };

    match ch_type {
        // A color's emitters, whether a definition files them under Intensity
        // (QLC+'s `IntensityRed`) or Colour.
        ChannelType::Intensity | ChannelType::Colour
            if channel.get_colour() != ChannelColour::None =>
        {
            emitter(channel.get_colour())
        }
        ChannelType::Intensity => {
            MapAction::Set(scale_u8(level_u8(color.dimmer), max_dimmer, true))
        }
        // A color wheel or macro channel: capabilities describing colors.
        // Some fixtures (or sub-effects like rings) group such a wheel as
        // Gobo; it engages only when its capabilities hold colors.
        ChannelType::Colour | ChannelType::Gobo => {
            if is_black(color.srgb) {
                MapAction::Hold
            } else {
                MapAction::Set(map_nearest_color_capability(channel, color.srgb).unwrap_or(0))
            }
        }
        // Pan and tilt go out exactly as the evaluator produced them. There used
        // to be a mirror here for fixtures within 28 degrees of upside-down
        // (`|rot_x - PI| < 0.5`), and it was wrong twice over: no renderer
        // applied it, so the console and the visualizer disagreed about where a
        // flipped head was pointing; and "hung upside down" is a fact about the
        // mount, which the venue graph makes explicit, not something to sniff
        // out of a stored angle with a threshold.
        ChannelType::Pan => {
            if state.position[0].is_nan() {
                MapAction::Hold
            } else {
                MapAction::Set(map_position_channel(
                    state.position[0],
                    pan_max_deg,
                    channel.preset.as_deref().unwrap_or(""),
                ))
            }
        }
        ChannelType::Tilt => {
            if state.position[1].is_nan() {
                MapAction::Hold
            } else {
                MapAction::Set(map_position_channel(
                    state.position[1],
                    tilt_max_deg,
                    channel.preset.as_deref().unwrap_or(""),
                ))
            }
        }
        ChannelType::Shutter => {
            // Shutter channels are owned by the strobe cascade and never reach
            // map_value (the fixture loop routes them through shutter_action).
            // Defensive fallback in case of refactor drift: hold the previous value.
            MapAction::Hold
        }
        ChannelType::Speed => {
            // Pan/tilt motor speed. Binary: 1.0 is fastest, 0.0 slowest. A
            // `...SlowFast` preset runs slow to fast up the channel; every
            // other speed channel (the `...FastSlow` presets, and most
            // fixtures without a preset) runs fast to slow.
            let rising = channel
                .preset
                .as_deref()
                .is_some_and(|preset| preset.ends_with("SlowFast"));
            let fastest = state.speed > 0.5;
            MapAction::Set(if fastest == rising { 255 } else { 0 })
        }
        _ => MapAction::Set(0),
    }
}

fn scale_u8(value: u8, scale: f32, enabled: bool) -> u8 {
    if !enabled {
        return value;
    }
    ((value as f32) * scale).round().clamp(0.0, 255.0) as u8
}

fn map_position_channel(pos_deg: f32, max_deg: f32, preset: &str) -> u8 {
    let max_deg = max_deg.max(1.0);
    // Semantic convention: `pos_deg` is signed and centered at 0.
    // - Pan range is approximately [-PanMax/2 .. +PanMax/2]
    // - Tilt range is approximately [-TiltMax/2 .. +TiltMax/2]
    // Map into DMX 0..1 by shifting into [0..max].
    let normalized = ((pos_deg + max_deg / 2.0) / max_deg).clamp(0.0, 1.0);
    let value_16 = (normalized * 65535.0).round() as u16;
    let msb = (value_16 >> 8) as u8;
    let lsb = (value_16 & 0xff) as u8;

    if preset.to_lowercase().contains("fine") {
        lsb
    } else {
        msb
    }
}

fn map_nearest_color_capability(
    channel: &crate::models::fixtures::Channel,
    desired_rgb: [f32; 3],
) -> Option<u8> {
    let slots: Vec<Slot> = channel
        .capabilities
        .iter()
        .filter_map(|cap| {
            Some(Slot {
                byte: cap.min,
                rgb: capability_rgb(cap)?,
            })
        })
        .collect();
    nearest_slot(&slots, desired_rgb).map(|slot| slot.byte)
}

fn capability_rgb(cap: &crate::models::fixtures::Capability) -> Option<[f32; 3]> {
    // QLC+ uses Res1/Res2 for ColorMacro/ColorDoubleMacro. Legacy fixtures might use Color/Color2.
    let primary = cap
        .res1
        .as_deref()
        .or(cap.color.as_deref())
        .or(cap.res.as_deref())?;

    // If it's a split/double color capability, approximate by averaging.
    let secondary = cap.res2.as_deref().or(cap.color_2.as_deref());

    let c1 = hex(primary)?;
    if let Some(s) = secondary {
        if let Some(c2) = hex(s) {
            return Some([
                (c1[0] + c2[0]) * 0.5,
                (c1[1] + c2[1]) * 0.5,
                (c1[2] + c2[2]) * 0.5,
            ]);
        }
    }

    Some(c1)
}

fn is_black(rgb: [f32; 3]) -> bool {
    rgb[0] <= 0.0001 && rgb[1] <= 0.0001 && rgb[2] <= 0.0001
}

/// A 0–1 level as a DMX byte.
fn level_u8(level: f32) -> u8 {
    (level.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// The emitters a fixture mode mixes color with, besides red, green and
/// blue.
#[derive(Clone, Copy, Debug, Default)]
struct Emitters {
    white: bool,
    amber: bool,
    lime: bool,
    /// Cyan, magenta and yellow filters on a white lamp, with no red, green
    /// or blue emitters.
    subtractive: bool,
}

impl Emitters {
    fn of(def: &FixtureDefinition, mode: &Mode) -> Self {
        let colours: Vec<ChannelColour> = mode
            .channels
            .iter()
            .filter_map(|mc| def.channels.iter().find(|c| c.name == mc.name))
            .filter(|c| matches!(c.get_type(), ChannelType::Intensity | ChannelType::Colour))
            .map(Channel::get_colour)
            .collect();
        let has = |colour| colours.contains(&colour);
        let additive =
            has(ChannelColour::Red) || has(ChannelColour::Green) || has(ChannelColour::Blue);
        Self {
            white: has(ChannelColour::White),
            amber: has(ChannelColour::Amber),
            lime: has(ChannelColour::Lime),
            subtractive: !additive
                && (has(ChannelColour::Cyan)
                    || has(ChannelColour::Magenta)
                    || has(ChannelColour::Yellow)),
        }
    }
}

/// One head's light in the terms its channels take. The head's color is
/// linear Rec. 2020; the fixture's red, green and blue emitters are assumed
/// to have sRGB primaries, and [`Gamut::split`] maps a color they cannot make
/// to the nearest one they can. Levels are linear light, as a DMX level
/// is to a linear fixture.
#[derive(Clone, Copy, Debug, Default)]
struct HeadColor {
    red: f32,
    green: f32,
    blue: f32,
    white: f32,
    amber: f32,
    lime: f32,
    cyan: f32,
    magenta: f32,
    yellow: f32,
    /// What a master dimmer channel gets.
    dimmer: f32,
    /// The color as linear sRGB at a peak of 1. A color wheel's slots,
    /// written as sRGB hex, are compared with it byte for byte, and its luma
    /// dims a wheel fixture, as before the working space was Rec. 2020.
    srgb: [f32; 3],
}

impl HeadColor {
    fn new(state: &PrimitiveState, emitters: Emitters, wheel: bool) -> Self {
        let (levels, brightness) =
            Gamut::SRGB.split(state.color.map(f64::from), f64::from(state.dimmer));
        let srgb = levels.map(|v| v as f32);
        // The emitters make the color, at a peak of 1; a master dimmer, where
        // there is one, makes its brightness.
        let mut color = Self {
            dimmer: if wheel {
                // A wheel cannot dim its color, so a dark color dims the lamp.
                brightness as f32 * luma(srgb)
            } else {
                brightness as f32
            },
            srgb,
            ..Self::default()
        };
        let mut rest = levels;
        if emitters.subtractive {
            // A filter passes 1 − its level of the white lamp.
            [color.cyan, color.magenta, color.yellow] = rest.map(|v| (1.0 - v) as f32);
            return color;
        }
        // Each extra emitter takes as much of the color as it can make; red,
        // green and blue make the rest. The sum is the same light.
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
            share as f32
        };
        color.white = take(emitters.white, WHITE_EMITTER);
        color.amber = take(emitters.amber, AMBER_EMITTER);
        color.lime = take(emitters.lime, LIME_EMITTER);
        [color.red, color.green, color.blue] = rest.map(|v| v as f32);
        color
    }

    /// The level of the emitter of `colour`, 0–1.
    fn level(&self, colour: ChannelColour) -> f32 {
        match colour {
            ChannelColour::Red => self.red,
            ChannelColour::Green => self.green,
            ChannelColour::Blue => self.blue,
            ChannelColour::White => self.white,
            ChannelColour::Amber => self.amber,
            ChannelColour::Lime => self.lime,
            ChannelColour::Cyan => self.cyan,
            ChannelColour::Magenta => self.magenta,
            ChannelColour::Yellow => self.yellow,
            ChannelColour::UV | ChannelColour::None => 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::fixtures::{Channel, Mode, ModeChannel};
    use crate::models::universe::{PrimitiveState, UniverseState};

    fn prim(dimmer: f32, r: f32, g: f32, b: f32, strobe: f32) -> PrimitiveState {
        PrimitiveState {
            dimmer,
            color: [r, g, b],
            strobe,
            position: [0.0, 0.0],
            speed: 0.0,
            aim: None,
        }
    }

    #[test]
    fn uses_mode_channel_number_for_addressing_and_prefers_fixture_dimmer_over_head() {
        let def = FixtureDefinition {
            library: None,
            manufacturer: "Test".into(),
            model: "Test".into(),
            type_: "Moving Head".into(),
            channels: vec![
                Channel {
                    lowered: None,
                    name: "Pan".into(),
                    preset: Some("PositionPan".into()),
                    group: None,
                    capabilities: vec![],
                },
                Channel {
                    lowered: None,
                    name: "Master Dimmer".into(),
                    preset: Some("IntensityMasterDimmer".into()),
                    group: None,
                    capabilities: vec![],
                },
                Channel {
                    lowered: None,
                    name: "Red".into(),
                    preset: Some("IntensityRed".into()),
                    group: None,
                    capabilities: vec![],
                },
            ],
            modes: vec![Mode {
                name: "TestMode".into(),
                // Intentionally out of order: channel number 1 comes before 0.
                channels: vec![
                    ModeChannel {
                        number: 1,
                        name: "Master Dimmer".into(),
                    },
                    ModeChannel {
                        number: 0,
                        name: "Pan".into(),
                    },
                    ModeChannel {
                        number: 2,
                        name: "Red".into(),
                    },
                ],
                heads: vec![crate::models::fixtures::Head {
                    // Put the dimmer and red channels inside the head.
                    // Master dimmer should still come from fixture primitive.
                    channels: vec![1, 2],
                }],
            }],
            physical: None,
        };

        let mut definitions = HashMap::new();
        definitions.insert("Test/Test.qxf".into(), def);

        let fixtures = vec![PatchedFixture {
            id: "fx".into(),
            uid: None,
            venue_id: "test-venue".into(),
            universe: 1,
            address: 1,
            num_channels: 3,
            manufacturer: "Test".into(),
            model: "Test".into(),
            mode_name: "TestMode".into(),
            fixture_path: "Test/Test.qxf".into(),
            label: None,
            address_pinned: false,
        }];

        let mut primitives = HashMap::new();
        // Fixture-level: dimmer on.
        primitives.insert("fx".into(), prim(1.0, 0.0, 0.0, 0.0, 0.0));
        // Head-level: dimmer off but red on (should apply to red channel).
        primitives.insert("fx:0".into(), prim(0.0, 1.0, 0.0, 0.0, 0.0));

        let state = UniverseState {
            primitives,
            lasers: Vec::new(),
        };

        let buffers = generate_dmx(&state, &fixtures, &definitions, None, 1.0, 0.0);
        let buf = buffers.get(&1).expect("universe buffer");

        // Pan is channel number 0 => DMX address 0 (0-based). With centered degrees, 0deg maps to midpoint.
        assert_eq!(buf[0], 128);
        // Dimmer is channel number 1 => DMX address 1 and should come from fixture primitive (255).
        assert_eq!(buf[1], 255);
        // Red is channel number 2 => DMX address 2 and should come from head primitive (255).
        assert_eq!(buf[2], 255);
    }

    #[test]
    fn falls_back_to_head0_when_fixture_primitive_missing() {
        let def = FixtureDefinition {
            library: None,
            manufacturer: "Test".into(),
            model: "Test".into(),
            type_: "Moving Head".into(),
            channels: vec![Channel {
                lowered: None,
                name: "Master Dimmer".into(),
                preset: Some("IntensityMasterDimmer".into()),
                group: None,
                capabilities: vec![],
            }],
            modes: vec![Mode {
                name: "TestMode".into(),
                channels: vec![ModeChannel {
                    number: 5,
                    name: "Master Dimmer".into(),
                }],
                // No <Head> entries in the mode
                heads: vec![],
            }],
            physical: None,
        };

        let mut definitions = HashMap::new();
        definitions.insert("Test/Test.qxf".into(), def);

        let fixtures = vec![PatchedFixture {
            id: "fx".into(),
            uid: None,
            venue_id: "test-venue".into(),
            universe: 1,
            address: 49,
            num_channels: 10,
            manufacturer: "Test".into(),
            model: "Test".into(),
            mode_name: "TestMode".into(),
            fixture_path: "Test/Test.qxf".into(),
            label: None,
            address_pinned: false,
        }];

        // Only head primitive exists (this matches how the current "select" node emits IDs)
        let mut primitives = HashMap::new();
        primitives.insert("fx:0".into(), prim(1.0, 0.0, 0.0, 0.0, 0.0));
        let state = UniverseState {
            primitives,
            lasers: Vec::new(),
        };

        let buffers = generate_dmx(&state, &fixtures, &definitions, None, 1.0, 0.0);
        let buf = buffers.get(&1).expect("universe buffer");

        // Start address 49 => 0-based 48. Channel number 5 => index 53 (DMX channel 54).
        assert_eq!(buf[53], 255);
    }

    /// A head's color on a wheel fixture with no master dimmer.
    fn wheel(state: &PrimitiveState) -> HeadColor {
        HeadColor::new(state, Emitters::default(), true)
    }

    #[test]
    fn maps_color_wheel_to_nearest_capability() {
        let channel = Channel {
            lowered: None,
            name: "Colors".into(),
            preset: None,
            group: Some(crate::models::fixtures::Group {
                byte: 0,
                value: "Colour".into(),
            }),
            capabilities: vec![
                crate::models::fixtures::Capability {
                    min: 0,
                    max: 9,
                    preset: Some("ColorMacro".into()),
                    res1: Some("#ffffff".into()),
                    res2: None,
                    res: None,
                    color: None,
                    color_2: None,
                    label: "White".into(),
                },
                crate::models::fixtures::Capability {
                    min: 10,
                    max: 19,
                    preset: Some("ColorMacro".into()),
                    res1: Some("#ff0000".into()),
                    res2: None,
                    res: None,
                    color: None,
                    color_2: None,
                    label: "Red".into(),
                },
                crate::models::fixtures::Capability {
                    min: 20,
                    max: 29,
                    preset: Some("ColorMacro".into()),
                    res1: Some("#00ff00".into()),
                    res2: None,
                    res: None,
                    color: None,
                    color_2: None,
                    label: "Green".into(),
                },
            ],
        };

        let state = prim(1.0, 0.95, 0.05, 0.05, 0.0);
        assert_eq!(
            // has_color_wheel = true since this is a color wheel test
            map_value(&channel, &state, &wheel(&state), 540.0, 270.0, 1.0, false),
            MapAction::Set(10)
        );

        let state = prim(1.0, 0.05, 0.95, 0.05, 0.0);
        assert_eq!(
            map_value(&channel, &state, &wheel(&state), 540.0, 270.0, 1.0, false),
            MapAction::Set(20)
        );
    }

    #[test]
    fn holds_wheel_value_when_color_is_black() {
        let channel = Channel {
            lowered: None,
            name: "Colors".into(),
            preset: None,
            group: Some(crate::models::fixtures::Group {
                byte: 0,
                value: "Colour".into(),
            }),
            capabilities: vec![
                crate::models::fixtures::Capability {
                    min: 0,
                    max: 9,
                    preset: Some("ColorMacro".into()),
                    res1: Some("#ffffff".into()),
                    res2: None,
                    res: None,
                    color: None,
                    color_2: None,
                    label: "White".into(),
                },
                crate::models::fixtures::Capability {
                    min: 10,
                    max: 19,
                    preset: Some("ColorMacro".into()),
                    res1: Some("#ff0000".into()),
                    res2: None,
                    res: None,
                    color: None,
                    color_2: None,
                    label: "Red".into(),
                },
            ],
        };

        // First frame sets red (10)
        let fixtures = vec![PatchedFixture {
            id: "fx".into(),
            uid: None,
            venue_id: "test-venue".into(),
            universe: 1,
            address: 1,
            num_channels: 1,
            manufacturer: "Test".into(),
            model: "Test".into(),
            mode_name: "TestMode".into(),
            fixture_path: "Test/Test.qxf".into(),
            label: None,
            address_pinned: false,
        }];

        let def = FixtureDefinition {
            library: None,
            manufacturer: "Test".into(),
            model: "Test".into(),
            type_: "Moving Head".into(),
            channels: vec![channel],
            modes: vec![Mode {
                name: "TestMode".into(),
                channels: vec![ModeChannel {
                    number: 0,
                    name: "Colors".into(),
                }],
                heads: vec![],
            }],
            physical: None,
        };

        let mut definitions = HashMap::new();
        definitions.insert("Test/Test.qxf".into(), def);

        let mut primitives = HashMap::new();
        primitives.insert("fx".into(), prim(1.0, 1.0, 0.0, 0.0, 0.0));
        let state = UniverseState {
            primitives,
            lasers: Vec::new(),
        };

        let buffers1 = generate_dmx(&state, &fixtures, &definitions, None, 1.0, 0.0);
        let prev = buffers1.get(&1).copied().unwrap();
        assert_eq!(prev[0], 10);

        // Second frame sends black -> hold previous wheel value
        let mut primitives = HashMap::new();
        primitives.insert("fx".into(), prim(1.0, 0.0, 0.0, 0.0, 0.0));
        let state2 = UniverseState {
            primitives,
            lasers: Vec::new(),
        };

        let mut prev_map = HashMap::new();
        prev_map.insert(1i64, prev);
        let buffers2 = generate_dmx(&state2, &fixtures, &definitions, Some(&prev_map), 1.0, 0.0);
        let buf2 = buffers2.get(&1).unwrap();
        assert_eq!(buf2[0], 10);
    }

    #[test]
    fn holds_pan_when_position_axis_is_nan() {
        let def = FixtureDefinition {
            library: None,
            manufacturer: "Test".into(),
            model: "Test".into(),
            type_: "Moving Head".into(),
            channels: vec![
                Channel {
                    lowered: None,
                    name: "Pan".into(),
                    preset: Some("PositionPan".into()),
                    group: None,
                    capabilities: vec![],
                },
                Channel {
                    lowered: None,
                    name: "Pan fine".into(),
                    preset: Some("PositionPanFine".into()),
                    group: None,
                    capabilities: vec![],
                },
            ],
            modes: vec![Mode {
                name: "TestMode".into(),
                channels: vec![
                    ModeChannel {
                        number: 0,
                        name: "Pan".into(),
                    },
                    ModeChannel {
                        number: 1,
                        name: "Pan fine".into(),
                    },
                ],
                heads: vec![],
            }],
            physical: None,
        };

        let mut definitions = HashMap::new();
        definitions.insert("Test/Test.qxf".into(), def);

        let fixtures = vec![PatchedFixture {
            id: "fx".into(),
            uid: None,
            venue_id: "test-venue".into(),
            universe: 1,
            address: 1,
            num_channels: 2,
            manufacturer: "Test".into(),
            model: "Test".into(),
            mode_name: "TestMode".into(),
            fixture_path: "Test/Test.qxf".into(),
            label: None,
            address_pinned: false,
        }];

        // Previous buffer has some pan value already set
        let mut prev_buf = [0u8; 512];
        prev_buf[0] = 123;
        prev_buf[1] = 45;
        let mut prev_map = HashMap::new();
        prev_map.insert(1i64, prev_buf);

        // Now emit NaN for pan axis -> should hold previous values
        let mut primitives = HashMap::new();
        let mut p = prim(1.0, 0.0, 0.0, 0.0, 0.0);
        p.position = [f32::NAN, 0.0];
        primitives.insert("fx".into(), p);
        let state = UniverseState {
            primitives,
            lasers: Vec::new(),
        };

        let buffers = generate_dmx(&state, &fixtures, &definitions, Some(&prev_map), 1.0, 0.0);
        let buf = buffers.get(&1).unwrap();
        assert_eq!(buf[0], 123);
        assert_eq!(buf[1], 45);
    }

    // ─────────────────────────────────────────────────────────────────────
    // Strobe cascade
    //
    // Helpers + test fixtures for the strobe rung tests below. Each test
    // builds the smallest fixture mode that exposes the actuator combination
    // we want to exercise, then asserts the cascade picks the right rung and
    // writes the expected DMX bytes.
    // ─────────────────────────────────────────────────────────────────────

    use crate::models::fixtures::{Capability, Head};

    fn dim_ch(name: &str) -> Channel {
        Channel {
            lowered: None,
            name: name.into(),
            preset: Some("IntensityMasterDimmer".into()),
            group: None,
            capabilities: vec![],
        }
    }
    fn rgb_chs() -> Vec<Channel> {
        vec![
            Channel {
                lowered: None,
                name: "Red".into(),
                preset: Some("IntensityRed".into()),
                group: None,
                capabilities: vec![],
            },
            Channel {
                lowered: None,
                name: "Green".into(),
                preset: Some("IntensityGreen".into()),
                group: None,
                capabilities: vec![],
            },
            Channel {
                lowered: None,
                name: "Blue".into(),
                preset: Some("IntensityBlue".into()),
                group: None,
                capabilities: vec![],
            },
        ]
    }
    fn shutter_ch_with_caps() -> Channel {
        Channel {
            lowered: None,
            name: "Shutter".into(),
            preset: Some("ShutterStrobe".into()),
            group: None,
            capabilities: vec![
                Capability {
                    min: 0,
                    max: 9,
                    preset: Some("ShutterOpen".into()),
                    res1: None,
                    res2: None,
                    res: None,
                    color: None,
                    color_2: None,
                    label: "Shutter Open".into(),
                },
                Capability {
                    min: 11,
                    max: 255,
                    preset: Some("StrobeSlowToFast".into()),
                    res1: None,
                    res2: None,
                    res: None,
                    color: None,
                    color_2: None,
                    label: "Strobe".into(),
                },
            ],
        }
    }
    fn shutter_ch_no_open_cap() -> Channel {
        Channel {
            lowered: None,
            name: "Shutter".into(),
            preset: Some("ShutterStrobe".into()),
            group: None,
            capabilities: vec![Capability {
                min: 10,
                max: 255,
                preset: Some("StrobeSlowToFast".into()),
                res1: None,
                res2: None,
                res: None,
                color: None,
                color_2: None,
                label: "Strobe".into(),
            }],
        }
    }
    fn fixture_at(id: &str, address: i64) -> PatchedFixture {
        PatchedFixture {
            id: id.into(),
            uid: None,
            venue_id: "v".into(),
            universe: 1,
            address,
            num_channels: 16,
            manufacturer: "T".into(),
            model: "T".into(),
            mode_name: "M".into(),
            fixture_path: "T/T.qxf".into(),
            label: None,
            address_pinned: false,
        }
    }

    /// Single-head fixture (no <Head> entries) with master Dimmer + Shutter.
    /// fixture-level strobe → MasterShutter rung → shutter channel gets a strobe
    /// capability value; dimmer is untouched.
    #[test]
    fn strobe_via_master_shutter_when_present() {
        let def = FixtureDefinition {
            library: None,
            manufacturer: "T".into(),
            model: "T".into(),
            type_: "PAR".into(),
            channels: vec![dim_ch("Dimmer"), shutter_ch_with_caps()],
            modes: vec![Mode {
                name: "M".into(),
                channels: vec![
                    ModeChannel {
                        number: 0,
                        name: "Dimmer".into(),
                    },
                    ModeChannel {
                        number: 1,
                        name: "Shutter".into(),
                    },
                ],
                heads: vec![],
            }],
            physical: None,
        };
        let mut defs = HashMap::new();
        defs.insert("T/T.qxf".into(), def);
        let mut primitives = HashMap::new();
        primitives.insert("fx".into(), prim(1.0, 0.0, 0.0, 0.0, 0.5));
        let state = UniverseState {
            primitives,
            lasers: Vec::new(),
        };
        let fixtures = vec![fixture_at("fx", 1)];

        let buffers = generate_dmx(&state, &fixtures, &defs, None, 1.0, 0.0);
        let buf = buffers.get(&1).unwrap();
        // Dimmer untouched (shutter does the strobing).
        assert_eq!(buf[0], 255);
        // Shutter: strobe cap min=11 max=255 range=244, strobe=0.5 -> 11 + 122 = 133.
        assert_eq!(buf[1], 133);
    }

    /// Master shutter + heads disagree → downgrade master shutter to Open and
    /// fall through to per-head dimmer gating.
    #[test]
    fn strobe_master_shutter_disagrees_falls_to_per_head_dimmer() {
        // 2 heads: each with its own Dimmer + R/G/B. Fixture has a master shutter.
        // Head 0: strobe=1.0 → strobing. Head 1: strobe=0.0 → steady.
        // ch 0: Shutter (master)
        // ch 1: Dimmer head 0
        // ch 2: Red head 0
        // ch 3: Dimmer head 1
        // ch 4: Red head 1
        let mut channels = vec![shutter_ch_with_caps(), dim_ch("Dim0")];
        channels.push(Channel {
            lowered: None,
            name: "Red0".into(),
            preset: Some("IntensityRed".into()),
            group: None,
            capabilities: vec![],
        });
        channels.push(dim_ch("Dim1"));
        channels.push(Channel {
            lowered: None,
            name: "Red1".into(),
            preset: Some("IntensityRed".into()),
            group: None,
            capabilities: vec![],
        });
        let def = FixtureDefinition {
            library: None,
            manufacturer: "T".into(),
            model: "T".into(),
            type_: "Bar".into(),
            channels,
            modes: vec![Mode {
                name: "M".into(),
                channels: vec![
                    ModeChannel {
                        number: 0,
                        name: "Shutter".into(),
                    },
                    ModeChannel {
                        number: 1,
                        name: "Dim0".into(),
                    },
                    ModeChannel {
                        number: 2,
                        name: "Red0".into(),
                    },
                    ModeChannel {
                        number: 3,
                        name: "Dim1".into(),
                    },
                    ModeChannel {
                        number: 4,
                        name: "Red1".into(),
                    },
                ],
                heads: vec![
                    Head {
                        channels: vec![1, 2],
                    },
                    Head {
                        channels: vec![3, 4],
                    },
                ],
            }],
            physical: None,
        };
        let mut defs = HashMap::new();
        defs.insert("T/T.qxf".into(), def);

        let mut primitives = HashMap::new();
        primitives.insert("fx:0".into(), prim(1.0, 1.0, 0.0, 0.0, 1.0));
        primitives.insert("fx:1".into(), prim(1.0, 1.0, 0.0, 0.0, 0.0));
        let state = UniverseState {
            primitives,
            lasers: Vec::new(),
        };
        let fixtures = vec![fixture_at("fx", 1)];

        // At t=0: gate ON (head 0 not gated yet).
        let buf_on = generate_dmx(&state, &fixtures, &defs, None, 1.0, 0.0)
            .remove(&1)
            .unwrap();
        // Master shutter: downgraded to Open (cap min 0).
        assert_eq!(buf_on[0], 0);
        // Head 0 dimmer: ON phase, full brightness.
        assert_eq!(buf_on[1], 255);
        // Head 1 dimmer: not strobing, full brightness.
        assert_eq!(buf_on[3], 255);

        // At t = half_period + epsilon (strobe=1.0 → hz=20 → period=0.05s, half=0.025s).
        let buf_off = generate_dmx(&state, &fixtures, &defs, None, 1.0, 0.026)
            .remove(&1)
            .unwrap();
        // Master shutter still Open.
        assert_eq!(buf_off[0], 0);
        // Head 0 dimmer: OFF phase, gated to 0.
        assert_eq!(buf_off[1], 0);
        // Head 1 dimmer: not strobing, unchanged.
        assert_eq!(buf_off[3], 255);
        // Head 0 Red: per-head dimmer rung gates the dimmer, NOT the color channels.
        // So Red0 stays at full (255 * 1.0).
        assert_eq!(buf_off[2], 255);
    }

    /// No shutter, master dimmer only, fixture-level strobe → MasterDimmer rung.
    #[test]
    fn strobe_via_master_dimmer_when_no_shutter() {
        let mut channels = vec![dim_ch("Dimmer")];
        channels.extend(rgb_chs());
        let def = FixtureDefinition {
            library: None,
            manufacturer: "T".into(),
            model: "T".into(),
            type_: "PAR".into(),
            channels,
            modes: vec![Mode {
                name: "M".into(),
                channels: vec![
                    ModeChannel {
                        number: 0,
                        name: "Dimmer".into(),
                    },
                    ModeChannel {
                        number: 1,
                        name: "Red".into(),
                    },
                    ModeChannel {
                        number: 2,
                        name: "Green".into(),
                    },
                    ModeChannel {
                        number: 3,
                        name: "Blue".into(),
                    },
                ],
                heads: vec![],
            }],
            physical: None,
        };
        let mut defs = HashMap::new();
        defs.insert("T/T.qxf".into(), def);
        let mut primitives = HashMap::new();
        primitives.insert("fx".into(), prim(1.0, 1.0, 0.0, 0.0, 1.0));
        let state = UniverseState {
            primitives,
            lasers: Vec::new(),
        };
        let fixtures = vec![fixture_at("fx", 1)];

        // t=0 → ON: dimmer at 255.
        let on = generate_dmx(&state, &fixtures, &defs, None, 1.0, 0.0)
            .remove(&1)
            .unwrap();
        assert_eq!(on[0], 255);
        // t=0.026 → OFF: dimmer gated to 0.
        let off = generate_dmx(&state, &fixtures, &defs, None, 1.0, 0.026)
            .remove(&1)
            .unwrap();
        assert_eq!(off[0], 0);
        // Red is NOT gated under MasterDimmer rung (the dimmer handles it).
        assert_eq!(off[1], 255);
    }

    /// RGB-only fixture (no master dimmer, no shutter), strobe → PerHeadColor /
    /// MasterColor rung. With no heads declared, the master color path applies.
    #[test]
    fn strobe_via_master_color_when_no_dimmer_or_shutter() {
        let def = FixtureDefinition {
            library: None,
            manufacturer: "T".into(),
            model: "T".into(),
            type_: "RGB".into(),
            channels: rgb_chs(),
            modes: vec![Mode {
                name: "M".into(),
                channels: vec![
                    ModeChannel {
                        number: 0,
                        name: "Red".into(),
                    },
                    ModeChannel {
                        number: 1,
                        name: "Green".into(),
                    },
                    ModeChannel {
                        number: 2,
                        name: "Blue".into(),
                    },
                ],
                heads: vec![],
            }],
            physical: None,
        };
        let mut defs = HashMap::new();
        defs.insert("T/T.qxf".into(), def);
        let mut primitives = HashMap::new();
        primitives.insert("fx".into(), prim(1.0, 1.0, 1.0, 1.0, 1.0));
        let state = UniverseState {
            primitives,
            lasers: Vec::new(),
        };
        let fixtures = vec![fixture_at("fx", 1)];

        let on = generate_dmx(&state, &fixtures, &defs, None, 1.0, 0.0)
            .remove(&1)
            .unwrap();
        // ON phase: full white.
        assert_eq!([on[0], on[1], on[2]], [255, 255, 255]);
        let off = generate_dmx(&state, &fixtures, &defs, None, 1.0, 0.026)
            .remove(&1)
            .unwrap();
        // OFF phase: all RGB gated to 0.
        assert_eq!([off[0], off[1], off[2]], [0, 0, 0]);
    }

    /// Shutter with explicit Open capability, strobe=0 → writes Open cap min (not blind 0).
    #[test]
    fn shutter_open_capability_used_when_strobe_zero() {
        let def = FixtureDefinition {
            library: None,
            manufacturer: "T".into(),
            model: "T".into(),
            type_: "PAR".into(),
            channels: vec![shutter_ch_with_caps()],
            modes: vec![Mode {
                name: "M".into(),
                channels: vec![ModeChannel {
                    number: 0,
                    name: "Shutter".into(),
                }],
                heads: vec![],
            }],
            physical: None,
        };
        let mut defs = HashMap::new();
        defs.insert("T/T.qxf".into(), def);
        let mut primitives = HashMap::new();
        primitives.insert("fx".into(), prim(0.0, 0.0, 0.0, 0.0, 0.0));
        let state = UniverseState {
            primitives,
            lasers: Vec::new(),
        };
        let fixtures = vec![fixture_at("fx", 1)];

        let buf = generate_dmx(&state, &fixtures, &defs, None, 1.0, 0.0)
            .remove(&1)
            .unwrap();
        // Open cap min = 0.
        assert_eq!(buf[0], 0);
    }

    /// Shutter channel WITHOUT an Open capability + strobe=0 → Hold the previous
    /// frame's value rather than blindly writing 0 (which could close a shutter
    /// on a fixture where 0 means closed).
    #[test]
    fn shutter_holds_when_no_open_capability_and_strobe_zero() {
        let def = FixtureDefinition {
            library: None,
            manufacturer: "T".into(),
            model: "T".into(),
            type_: "PAR".into(),
            channels: vec![shutter_ch_no_open_cap()],
            modes: vec![Mode {
                name: "M".into(),
                channels: vec![ModeChannel {
                    number: 0,
                    name: "Shutter".into(),
                }],
                heads: vec![],
            }],
            physical: None,
        };
        let mut defs = HashMap::new();
        defs.insert("T/T.qxf".into(), def);
        let mut primitives = HashMap::new();
        primitives.insert("fx".into(), prim(0.0, 0.0, 0.0, 0.0, 0.0));
        let state = UniverseState {
            primitives,
            lasers: Vec::new(),
        };
        let fixtures = vec![fixture_at("fx", 1)];

        // Prev buffer carries shutter at 200 — should hold.
        let mut prev = [0u8; 512];
        prev[0] = 200;
        let mut prev_map = HashMap::new();
        prev_map.insert(1i64, prev);

        let buf = generate_dmx(&state, &fixtures, &defs, Some(&prev_map), 1.0, 0.0)
            .remove(&1)
            .unwrap();
        assert_eq!(buf[0], 200);
    }

    /// Strobe cascade must NEVER gate Pan/Tilt — movement should keep playing
    /// while the fixture flickers.
    #[test]
    fn strobe_does_not_gate_pan_tilt() {
        let def = FixtureDefinition {
            library: None,
            manufacturer: "T".into(),
            model: "T".into(),
            type_: "Mover".into(),
            channels: vec![
                Channel {
                    lowered: None,
                    name: "Pan".into(),
                    preset: Some("PositionPan".into()),
                    group: None,
                    capabilities: vec![],
                },
                Channel {
                    lowered: None,
                    name: "Tilt".into(),
                    preset: Some("PositionTilt".into()),
                    group: None,
                    capabilities: vec![],
                },
                dim_ch("Dimmer"),
            ],
            modes: vec![Mode {
                name: "M".into(),
                channels: vec![
                    ModeChannel {
                        number: 0,
                        name: "Pan".into(),
                    },
                    ModeChannel {
                        number: 1,
                        name: "Tilt".into(),
                    },
                    ModeChannel {
                        number: 2,
                        name: "Dimmer".into(),
                    },
                ],
                heads: vec![],
            }],
            physical: None,
        };
        let mut defs = HashMap::new();
        defs.insert("T/T.qxf".into(), def);
        let mut primitives = HashMap::new();
        primitives.insert("fx".into(), prim(1.0, 0.0, 0.0, 0.0, 1.0));
        let state = UniverseState {
            primitives,
            lasers: Vec::new(),
        };
        let fixtures = vec![fixture_at("fx", 1)];

        // OFF phase: dimmer should gate, pan/tilt stay at 128 (0 deg with default range).
        let off = generate_dmx(&state, &fixtures, &defs, None, 1.0, 0.026)
            .remove(&1)
            .unwrap();
        assert_eq!(off[0], 128); // Pan unchanged
        assert_eq!(off[1], 128); // Tilt unchanged
        assert_eq!(off[2], 0); // Dimmer gated
    }

    /// strobe == 0 with no master shutter → no gating; dimmer passes through at any time.
    #[test]
    fn strobe_zero_no_gating_applied() {
        let mut channels = vec![dim_ch("Dimmer")];
        channels.extend(rgb_chs());
        let def = FixtureDefinition {
            library: None,
            manufacturer: "T".into(),
            model: "T".into(),
            type_: "PAR".into(),
            channels,
            modes: vec![Mode {
                name: "M".into(),
                channels: vec![
                    ModeChannel {
                        number: 0,
                        name: "Dimmer".into(),
                    },
                    ModeChannel {
                        number: 1,
                        name: "Red".into(),
                    },
                    ModeChannel {
                        number: 2,
                        name: "Green".into(),
                    },
                    ModeChannel {
                        number: 3,
                        name: "Blue".into(),
                    },
                ],
                heads: vec![],
            }],
            physical: None,
        };
        let mut defs = HashMap::new();
        defs.insert("T/T.qxf".into(), def);
        let mut primitives = HashMap::new();
        primitives.insert("fx".into(), prim(1.0, 1.0, 1.0, 1.0, 0.0));
        let state = UniverseState {
            primitives,
            lasers: Vec::new(),
        };
        let fixtures = vec![fixture_at("fx", 1)];

        for t in [0.0_f64, 0.026, 0.05, 0.1] {
            let buf = generate_dmx(&state, &fixtures, &defs, None, 1.0, t)
                .remove(&1)
                .unwrap();
            assert_eq!(
                [buf[0], buf[1], buf[2], buf[3]],
                [255, 255, 255, 255],
                "no gating expected at t={t}"
            );
        }
    }

    /// Heads agreeing on a master shutter fixture → use the master shutter at
    /// the unified strobe value; no per-head dimmer gating happens.
    #[test]
    fn multihead_agreeing_strobe_uses_master_shutter() {
        // 2 heads with R/G/B each, plus a master shutter. Both heads strobe=0.5.
        let mut channels = vec![shutter_ch_with_caps()];
        for i in 0..2 {
            channels.push(Channel {
                lowered: None,
                name: format!("R{i}"),
                preset: Some("IntensityRed".into()),
                group: None,
                capabilities: vec![],
            });
            channels.push(Channel {
                lowered: None,
                name: format!("G{i}"),
                preset: Some("IntensityGreen".into()),
                group: None,
                capabilities: vec![],
            });
            channels.push(Channel {
                lowered: None,
                name: format!("B{i}"),
                preset: Some("IntensityBlue".into()),
                group: None,
                capabilities: vec![],
            });
        }
        let mut mode_channels = vec![ModeChannel {
            number: 0,
            name: "Shutter".into(),
        }];
        for (i, head_offset) in [(0, 1), (1, 4)].iter() {
            mode_channels.push(ModeChannel {
                number: *head_offset as u32,
                name: format!("R{i}"),
            });
            mode_channels.push(ModeChannel {
                number: (head_offset + 1) as u32,
                name: format!("G{i}"),
            });
            mode_channels.push(ModeChannel {
                number: (head_offset + 2) as u32,
                name: format!("B{i}"),
            });
        }
        let def = FixtureDefinition {
            library: None,
            manufacturer: "T".into(),
            model: "T".into(),
            type_: "Bar".into(),
            channels,
            modes: vec![Mode {
                name: "M".into(),
                channels: mode_channels,
                heads: vec![
                    Head {
                        channels: vec![1, 2, 3],
                    },
                    Head {
                        channels: vec![4, 5, 6],
                    },
                ],
            }],
            physical: None,
        };
        let mut defs = HashMap::new();
        defs.insert("T/T.qxf".into(), def);
        let mut primitives = HashMap::new();
        primitives.insert("fx:0".into(), prim(1.0, 1.0, 0.0, 0.0, 0.5));
        primitives.insert("fx:1".into(), prim(1.0, 1.0, 0.0, 0.0, 0.5));
        let state = UniverseState {
            primitives,
            lasers: Vec::new(),
        };
        let fixtures = vec![fixture_at("fx", 1)];

        // Across multiple times, the master shutter should carry the strobe and
        // per-head colors should NEVER be gated (the shutter is doing it).
        for t in [0.0_f64, 0.026, 0.06] {
            let buf = generate_dmx(&state, &fixtures, &defs, None, 1.0, t)
                .remove(&1)
                .unwrap();
            assert_eq!(buf[0], 133, "master shutter at unified 0.5 at t={t}");
            // Head 0 Red.
            assert_eq!(buf[1], 255, "head 0 red unchanged at t={t}");
            // Head 1 Red.
            assert_eq!(buf[4], 255, "head 1 red unchanged at t={t}");
        }
    }

    #[test]
    fn speed_presets_are_speed_and_run_fastest_by_their_direction() {
        let speed = |preset: &str| Channel {
            lowered: None,
            name: "Speed".into(),
            preset: Some(preset.into()),
            group: None,
            capabilities: vec![],
        };
        let fast = PrimitiveState {
            speed: 1.0,
            ..prim(1.0, 1.0, 1.0, 1.0, 0.0)
        };
        for (preset, fastest) in [
            ("SpeedPanTiltFastSlow", 0),
            ("SpeedPanSlowFast", 255),
            ("SpeedTiltSlowFast", 255),
        ] {
            let channel = speed(preset);
            assert_eq!(channel.get_type(), ChannelType::Speed, "{preset}");
            assert_eq!(
                map_value(
                    &channel,
                    &fast,
                    &HeadColor::new(&fast, Emitters::default(), false),
                    540.0,
                    270.0,
                    1.0,
                    true
                ),
                MapAction::Set(fastest),
                "{preset}"
            );
        }
    }

    // -- color into emitters ------------------------------------------------

    fn intensity(name: &str) -> Channel {
        Channel {
            lowered: None,
            name: name.into(),
            preset: Some(format!("Intensity{name}")),
            group: None,
            capabilities: vec![],
        }
    }

    /// One fixture of the named intensity channels, in order, lit by a head
    /// of `color` (linear Rec. 2020) at `dimmer`. The DMX bytes, in order.
    fn drive(names: &[&str], color: [f64; 3], dimmer: f32) -> Vec<u8> {
        let channels: Vec<Channel> = names.iter().map(|name| intensity(name)).collect();
        let def = FixtureDefinition {
            library: None,
            manufacturer: "T".into(),
            model: "T".into(),
            type_: "LED".into(),
            modes: vec![Mode {
                name: "M".into(),
                channels: channels
                    .iter()
                    .enumerate()
                    .map(|(number, c)| ModeChannel {
                        number: number as u32,
                        name: c.name.clone(),
                    })
                    .collect(),
                heads: vec![],
            }],
            channels,
            physical: None,
        };
        let defs = HashMap::from([("T/T.qxf".to_string(), def)]);
        let head = luma_patterns::FixtureOutput::from_rgb(color);
        let [r, g, b] = head.color.unwrap().map(|v| v as f32);
        let peak = head.dimmer.unwrap() as f32;
        let state = UniverseState {
            primitives: HashMap::from([("fx".into(), prim(peak * dimmer, r, g, b, 0.0))]),
            lasers: Vec::new(),
        };
        let buffer = generate_dmx(&state, &[fixture_at("fx", 1)], &defs, None, 1.0, 0.0)
            .remove(&1)
            .unwrap();
        buffer[..names.len()].to_vec()
    }

    fn srgb(rgb: [f64; 3]) -> [f64; 3] {
        luma_patterns::color_space::from_srgb(rgb)
    }

    /// A stored clip drives an sRGB-primary RGB fixture as it did before
    /// the working space was Rec. 2020: its old numbers were linear sRGB, and
    /// the row conversion changes primaries only.
    #[test]
    fn a_converted_color_drives_the_dmx_it_did_before() {
        let orange = luma_patterns::color_space::from_linear_srgb([1.0, 0.5, 0.0]);
        let names = ["MasterDimmer", "Red", "Green", "Blue"];
        assert_eq!(drive(&names, orange, 1.0), [255, 255, 128, 0]);
        assert_eq!(drive(&names, orange, 0.5), [128, 255, 128, 0]);
        // Without a master dimmer the emitters get the color, as before.
        assert_eq!(drive(&["Red", "Green", "Blue"], orange, 0.5), [255, 128, 0]);
    }

    /// A green deeper than sRGB is the nearest green the fixture has, at the
    /// same lightness, not a wrapped or clipped channel: green leads, and red
    /// and blue stay low.
    #[test]
    fn a_color_outside_the_fixture_is_its_nearest_color() {
        let [dimmer, red, green, blue] = drive(
            &["MasterDimmer", "Red", "Green", "Blue"],
            [0.0, 1.0, 0.0],
            1.0,
        )[..] else {
            unreachable!()
        };
        assert!(dimmer > 128 && green == 255, "{dimmer} {green}");
        assert!(red < 64 && blue < 64, "{red} {blue}");
    }

    /// A white emitter makes the white part of a color; red, green and blue
    /// make the rest. An amber takes what it can of an orange.
    #[test]
    fn extra_emitters_take_their_share() {
        let names = ["MasterDimmer", "Red", "Green", "Blue", "White"];
        assert_eq!(
            drive(&names, srgb([1.0, 1.0, 1.0]), 1.0),
            [255, 0, 0, 0, 255]
        );
        assert_eq!(
            drive(&names, srgb([1.0, 0.0, 0.0]), 1.0),
            [255, 255, 0, 0, 0]
        );
        let names = ["MasterDimmer", "Red", "Green", "Blue", "Amber"];
        let [_, red, green, _, amber] = drive(&names, srgb([1.0, 0.5, 0.0]), 1.0)[..] else {
            unreachable!()
        };
        assert!(
            amber == 255 && green > 0 && red == 0,
            "{red} {green} {amber}"
        );
    }

    /// Cyan, magenta and yellow filter a white lamp: white is no filter,
    /// red is magenta and yellow, which take out green and blue.
    #[test]
    fn cmy_filters_subtract() {
        let names = ["MasterDimmer", "Cyan", "Magenta", "Yellow"];
        assert_eq!(drive(&names, srgb([1.0, 1.0, 1.0]), 1.0), [255, 0, 0, 0]);
        assert_eq!(
            drive(&names, srgb([1.0, 0.0, 0.0]), 1.0),
            [255, 0, 255, 255]
        );
    }

    #[test]
    fn a_mode_with_a_strobe_channel_strobes_on_its_own_clock() {
        let path = crate::resources::source_fixtures_root()
            .unwrap()
            .join("Venue/Venue-Tetra-Bar.qxf");
        let def = crate::fixtures::parser::parse_definition(&path).unwrap();
        assert!(strobes_on_own_clock(&def, "24-CH"));
        assert!(!strobes_on_own_clock(&def, "3-CH"));
        assert!(!strobes_on_own_clock(&def, "no such mode"));
    }
}
