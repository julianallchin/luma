use crate::models::fixtures::{ChannelColour, ChannelType, FixtureDefinition, PatchedFixture};
use crate::models::universe::{PrimitiveState, UniverseState};
use std::collections::HashMap;

pub fn generate_dmx(
    state: &UniverseState,
    fixtures: &[PatchedFixture],
    definitions: &HashMap<String, FixtureDefinition>,
    previous_universe_buffers: Option<&HashMap<i64, [u8; 512]>>,
    max_dimmer: f32,
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

        let has_master_dimmer = mode.channels.iter().any(|mode_channel| {
            let channel = match def.channels.iter().find(|c| c.name == mode_channel.name) {
                Some(c) => c,
                None => return false,
            };
            channel.get_type() == ChannelType::Intensity
                && channel.get_colour() == ChannelColour::None
        });

        let has_color_wheel = def.has_color_wheel(mode);

        let pan_max = def
            .physical
            .as_ref()
            .and_then(|p| p.focus.as_ref())
            .and_then(|f| f.pan_max)
            .unwrap_or(540) as f32;
        let tilt_max = def
            .physical
            .as_ref()
            .and_then(|p| p.focus.as_ref())
            .and_then(|f| f.tilt_max)
            .unwrap_or(270) as f32;

        // Determine if pan/tilt should be inverted based on fixture orientation.
        // When a fixture is flipped upside down (rot_x ≈ π), both pan and tilt
        // axes are effectively mirrored relative to the default ceiling-mount orientation.
        let is_flipped = (fixture.rot_x - std::f64::consts::PI).abs() < 0.5;
        let invert_pan = is_flipped;
        let invert_tilt = is_flipped;

        let buffer = buffers.entry(fixture.universe).or_insert([0; 512]);
        let prev = previous_universe_buffers.and_then(|m| m.get(&fixture.universe));

        // Map channel index to head index
        let mut channel_to_head: HashMap<u32, usize> = HashMap::new();
        for (head_idx, head) in mode.heads.iter().enumerate() {
            for &channel_idx in &head.channels {
                channel_to_head.insert(channel_idx, head_idx);
            }
        }

        for mode_channel in &mode.channels {
            let channel_number = mode_channel.number as usize;
            let dmx_address = (fixture.address - 1) as usize + channel_number;
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
            let head_idx = channel_to_head.get(&mode_channel.number);
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

            match map_value(
                channel,
                prim,
                pan_max,
                tilt_max,
                max_dimmer,
                has_master_dimmer,
                has_color_wheel,
                invert_pan,
                invert_tilt,
            ) {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MapAction {
    Set(u8),
    Hold,
}

fn map_value(
    channel: &crate::models::fixtures::Channel,
    state: &PrimitiveState,
    pan_max_deg: f32,
    tilt_max_deg: f32,
    max_dimmer: f32,
    has_master_dimmer: bool,
    has_color_wheel: bool,
    invert_pan: bool,
    invert_tilt: bool,
) -> MapAction {
    let ch_type = channel.get_type();

    match ch_type {
        ChannelType::Intensity => {
            // Check if it's a specific color intensity (some fixtures have "Red" channel type as Intensity)
            // But get_type() usually separates Colour from Intensity.
            // However, QLC+ might tag Red as IntensityRed preset.
            // My get_type logic: IntensityRed -> Intensity.
            // So I need to check colour too.

            MapAction::Set(match channel.get_colour() {
                ChannelColour::Red => scale_u8(
                    (state.color[0] * 255.0) as u8,
                    max_dimmer,
                    !has_master_dimmer,
                ),
                ChannelColour::Green => scale_u8(
                    (state.color[1] * 255.0) as u8,
                    max_dimmer,
                    !has_master_dimmer,
                ),
                ChannelColour::Blue => scale_u8(
                    (state.color[2] * 255.0) as u8,
                    max_dimmer,
                    !has_master_dimmer,
                ),
                ChannelColour::White => 0, // TODO: Add white support to PrimitiveState
                ChannelColour::Amber => 0,
                ChannelColour::UV => 0,
                ChannelColour::None => {
                    // Master Dimmer - for color wheel fixtures, multiply by color luminance
                    // since the wheel can't represent brightness
                    let dimmer = if has_color_wheel {
                        state.dimmer * color_luminance(state.color)
                    } else {
                        state.dimmer
                    };
                    scale_u8((dimmer * 255.0) as u8, max_dimmer, true)
                }
                _ => 0,
            })
        }
        ChannelType::Colour => {
            // Colour group can be:
            // - RGB/CMY/etc mixer channels (rarely tagged as Colour in QXF; often Intensity*)
            // - Color wheel / color macro channel with capabilities describing colors
            match channel.get_colour() {
                ChannelColour::Red => MapAction::Set(scale_u8(
                    (state.color[0] * 255.0) as u8,
                    max_dimmer,
                    !has_master_dimmer,
                )),
                ChannelColour::Green => MapAction::Set(scale_u8(
                    (state.color[1] * 255.0) as u8,
                    max_dimmer,
                    !has_master_dimmer,
                )),
                ChannelColour::Blue => MapAction::Set(scale_u8(
                    (state.color[2] * 255.0) as u8,
                    max_dimmer,
                    !has_master_dimmer,
                )),
                ChannelColour::White => MapAction::Set(0),
                ChannelColour::Amber => MapAction::Set(0),
                ChannelColour::UV => MapAction::Set(0),
                ChannelColour::None => {
                    if is_black(state.color) {
                        MapAction::Hold
                    } else {
                        MapAction::Set(
                            map_nearest_color_capability(channel, state.color).unwrap_or(0),
                        )
                    }
                }
                _ => MapAction::Set(0),
            }
        }
        ChannelType::Gobo => {
            // Some fixtures (or sub-effects like rings) represent "colors" via a wheel channel
            // grouped as Gobo. Only engage this mapping if capability resources contain colors.
            if is_black(state.color) {
                return MapAction::Hold;
            }
            if let Some(v) = map_nearest_color_capability(channel, state.color) {
                return MapAction::Set(v);
            }
            MapAction::Set(0)
        }
        ChannelType::Pan => {
            if state.position[0].is_nan() {
                MapAction::Hold
            } else {
                let pan_deg = if invert_pan {
                    -state.position[0]
                } else {
                    state.position[0]
                };
                MapAction::Set(map_position_channel(
                    pan_deg,
                    pan_max_deg,
                    channel.preset.as_deref().unwrap_or(""),
                ))
            }
        }
        ChannelType::Tilt => {
            if state.position[1].is_nan() {
                MapAction::Hold
            } else {
                let tilt_deg = if invert_tilt {
                    -state.position[1]
                } else {
                    state.position[1]
                };
                MapAction::Set(map_position_channel(
                    tilt_deg,
                    tilt_max_deg,
                    channel.preset.as_deref().unwrap_or(""),
                ))
            }
        }
        ChannelType::Shutter => {
            // Strobe logic
            // We need to find a capability that matches "Strobe" and map the value.
            // Or just simple mapping if no capabilities defined (generic dimmer/strobe).

            // 1. Generic mapping if no capabilities
            if channel.capabilities.is_empty() {
                if state.strobe > 0.0 {
                    // Simple map 10-255
                    return MapAction::Set(((state.strobe * 245.0) + 10.0) as u8);
                } else {
                    return MapAction::Set(0); // Open/Closed? Usually 0 is open or closed depending on fixture.
                                              // Actually, for Shutter channel:
                                              // 0-X is often Closed or Open.
                                              // Usually 0-10 Closed, 11-255 Open/Strobe.
                                              // OR 0-10 Open, 11-255 Strobe.
                                              // Safer to check capability.
                }
            }

            // 2. Capability Search
            // We want a capability that looks like "Strobe".
            // If state.strobe > 0, we want "Strobe".
            // If state.strobe == 0, we want "Open" (Shutter Open) or "Off" (Strobe Off).

            if state.strobe > 0.0 {
                // Find strobe capability
                if let Some(cap) = channel.capabilities.iter().find(|c| c.is_strobe()) {
                    // Map state.strobe (0.0-1.0) to cap.min-cap.max
                    let range = (cap.max - cap.min) as f32;
                    let val = cap.min as f32 + (state.strobe * range);
                    return MapAction::Set(val.clamp(cap.min as f32, cap.max as f32) as u8);
                }

                // Fallback: if no specific strobe capability found, but we are in Shutter channel,
                // try to find "Strobe" string in any capability.
                // My is_strobe() helper does this.

                // If absolutely nothing found, return linear map?
                return MapAction::Set(((state.strobe * 245.0) + 10.0) as u8);
            } else {
                // Strobe is 0 -> Shutter Open / Strobe Off.
                // Look for "Open", "On", "Off" (Strobe Off).
                // QLC+ often uses preset "ShutterOpen".
                if let Some(cap) = channel.capabilities.iter().find(|c| {
                    let p = c.preset.as_deref().unwrap_or("");
                    let l = c.label.to_lowercase();
                    p.contains("Open")
                        || l.contains("open")
                        || p.contains("LampOn")
                        || l.contains("shutter open")
                }) {
                    return MapAction::Set(cap.min); // Return start of Open range
                }

                // Fallback for "Shutter" channel: 255 is often Open. 0 might be Closed.
                // But some fixtures 0 is Open.
                // We'll default to 0 if we can't find "Open".
                // Actually, if we defaulted Strobe>0 to linear, we imply 0 is off.
                return MapAction::Set(0);
            }
        }
        ChannelType::Speed => {
            // Pan/Tilt Speed channel
            // Most fixtures: 0 = fastest, 255 = slowest (inverted)
            // Our binary: 0.0 = frozen, 1.0 = fast
            // Map: frozen (0.0) -> 255 (slowest), fast (1.0) -> 0 (fastest)
            if state.speed > 0.5 {
                MapAction::Set(0) // Fast = DMX 0 (fastest)
            } else {
                MapAction::Set(255) // Frozen = DMX 255 (slowest)
            }
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
    let mut best: Option<(f32, u8)> = None;

    for cap in &channel.capabilities {
        let Some(rgb) = capability_rgb(cap) else {
            continue;
        };
        let d = perceptual_color_distance(rgb, desired_rgb);
        let value = cap.min;

        match best {
            None => best = Some((d, value)),
            Some((best_d, _)) if d < best_d => best = Some((d, value)),
            _ => {}
        }
    }

    best.map(|(_, v)| v)
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

    let c1 = parse_hex_color(primary)?;
    if let Some(s) = secondary {
        if let Some(c2) = parse_hex_color(s) {
            return Some([
                (c1[0] + c2[0]) * 0.5,
                (c1[1] + c2[1]) * 0.5,
                (c1[2] + c2[2]) * 0.5,
            ]);
        }
    }

    Some(c1)
}

fn parse_hex_color(s: &str) -> Option<[f32; 3]> {
    // Expect "#RRGGBB"
    let s = s.trim();
    let hex = s.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()? as f32 / 255.0;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()? as f32 / 255.0;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()? as f32 / 255.0;
    Some([r, g, b])
}

fn color_distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    let dr = a[0] - b[0];
    let dg = a[1] - b[1];
    let db = a[2] - b[2];
    dr * dr + dg * dg + db * db
}

/// Perceptual color distance that considers saturation.
/// Desaturated colors (grays) should match white/neutral colors on wheels,
/// not saturated colors that happen to be close in RGB space.
fn perceptual_color_distance(wheel_rgb: [f32; 3], desired_rgb: [f32; 3]) -> f32 {
    // Calculate saturation (HSV-style): (max - min) / max
    fn saturation(rgb: [f32; 3]) -> f32 {
        let max = rgb[0].max(rgb[1]).max(rgb[2]);
        let min = rgb[0].min(rgb[1]).min(rgb[2]);
        if max < 0.0001 {
            0.0
        } else {
            (max - min) / max
        }
    }

    let desired_sat = saturation(desired_rgb);
    let wheel_sat = saturation(wheel_rgb);

    // Base RGB distance
    let rgb_dist = color_distance(wheel_rgb, desired_rgb);

    // Saturation difference penalty:
    // If desired color is desaturated (gray-ish), strongly prefer desaturated wheel colors
    // If desired color is saturated, prefer matching hue (RGB distance handles this)
    let sat_diff = (desired_sat - wheel_sat).abs();

    // Weight saturation matching more heavily for desaturated colors
    // When desired_sat is low, we want wheel_sat to also be low
    let sat_penalty = if desired_sat < 0.3 {
        // For grays: heavily penalize saturated wheel colors
        wheel_sat * wheel_sat * 2.0
    } else {
        // For saturated colors: small penalty for saturation mismatch
        sat_diff * 0.5
    };

    rgb_dist + sat_penalty
}

fn is_black(rgb: [f32; 3]) -> bool {
    rgb[0] <= 0.0001 && rgb[1] <= 0.0001 && rgb[2] <= 0.0001
}

/// Returns the perceived luminance of an RGB color (0.0 to 1.0).
/// Uses the standard luminance coefficients for sRGB.
fn color_luminance(rgb: [f32; 3]) -> f32 {
    0.299 * rgb[0] + 0.587 * rgb[1] + 0.114 * rgb[2]
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
        }
    }

    #[test]
    fn uses_mode_channel_number_for_addressing_and_prefers_fixture_dimmer_over_head() {
        let def = FixtureDefinition {
            manufacturer: "Test".into(),
            model: "Test".into(),
            type_: "Moving Head".into(),
            channels: vec![
                Channel {
                    name: "Pan".into(),
                    preset: Some("PositionPan".into()),
                    group: None,
                    capabilities: vec![],
                },
                Channel {
                    name: "Master Dimmer".into(),
                    preset: Some("IntensityMasterDimmer".into()),
                    group: None,
                    capabilities: vec![],
                },
                Channel {
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
            remote_id: None,
            uid: None,
            venue_id: 1,
            universe: 1,
            address: 1,
            num_channels: 3,
            manufacturer: "Test".into(),
            model: "Test".into(),
            mode_name: "TestMode".into(),
            fixture_path: "Test/Test.qxf".into(),
            label: None,
            pos_x: 0.0,
            pos_y: 0.0,
            pos_z: 0.0,
            rot_x: 0.0,
            rot_y: 0.0,
            rot_z: 0.0,
        }];

        let mut primitives = HashMap::new();
        // Fixture-level: dimmer on.
        primitives.insert("fx".into(), prim(1.0, 0.0, 0.0, 0.0, 0.0));
        // Head-level: dimmer off but red on (should apply to red channel).
        primitives.insert("fx:0".into(), prim(0.0, 1.0, 0.0, 0.0, 0.0));

        let state = UniverseState { primitives };

        let buffers = generate_dmx(&state, &fixtures, &definitions, None, 1.0);
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
            manufacturer: "Test".into(),
            model: "Test".into(),
            type_: "Moving Head".into(),
            channels: vec![Channel {
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
            remote_id: None,
            uid: None,
            venue_id: 1,
            universe: 1,
            address: 49,
            num_channels: 10,
            manufacturer: "Test".into(),
            model: "Test".into(),
            mode_name: "TestMode".into(),
            fixture_path: "Test/Test.qxf".into(),
            label: None,
            pos_x: 0.0,
            pos_y: 0.0,
            pos_z: 0.0,
            rot_x: 0.0,
            rot_y: 0.0,
            rot_z: 0.0,
        }];

        // Only head primitive exists (this matches how the current "select" node emits IDs)
        let mut primitives = HashMap::new();
        primitives.insert("fx:0".into(), prim(1.0, 0.0, 0.0, 0.0, 0.0));
        let state = UniverseState { primitives };

        let buffers = generate_dmx(&state, &fixtures, &definitions, None, 1.0);
        let buf = buffers.get(&1).expect("universe buffer");

        // Start address 49 => 0-based 48. Channel number 5 => index 53 (DMX channel 54).
        assert_eq!(buf[53], 255);
    }

    #[test]
    fn maps_color_wheel_to_nearest_capability() {
        let channel = Channel {
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
            map_value(&channel, &state, 540.0, 270.0, 1.0, false, true, false, false),
            MapAction::Set(10)
        );

        let state = prim(1.0, 0.05, 0.95, 0.05, 0.0);
        assert_eq!(
            map_value(&channel, &state, 540.0, 270.0, 1.0, false, true, false, false),
            MapAction::Set(20)
        );
    }

    #[test]
    fn holds_wheel_value_when_color_is_black() {
        let channel = Channel {
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
            remote_id: None,
            uid: None,
            venue_id: 1,
            universe: 1,
            address: 1,
            num_channels: 1,
            manufacturer: "Test".into(),
            model: "Test".into(),
            mode_name: "TestMode".into(),
            fixture_path: "Test/Test.qxf".into(),
            label: None,
            pos_x: 0.0,
            pos_y: 0.0,
            pos_z: 0.0,
            rot_x: 0.0,
            rot_y: 0.0,
            rot_z: 0.0,
        }];

        let def = FixtureDefinition {
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
        let state = UniverseState { primitives };

        let buffers1 = generate_dmx(&state, &fixtures, &definitions, None, 1.0);
        let prev = buffers1.get(&1).copied().unwrap();
        assert_eq!(prev[0], 10);

        // Second frame sends black -> hold previous wheel value
        let mut primitives = HashMap::new();
        primitives.insert("fx".into(), prim(1.0, 0.0, 0.0, 0.0, 0.0));
        let state2 = UniverseState { primitives };

        let mut prev_map = HashMap::new();
        prev_map.insert(1i64, prev);
        let buffers2 = generate_dmx(&state2, &fixtures, &definitions, Some(&prev_map), 1.0);
        let buf2 = buffers2.get(&1).unwrap();
        assert_eq!(buf2[0], 10);
    }

    #[test]
    fn holds_pan_when_position_axis_is_nan() {
        let def = FixtureDefinition {
            manufacturer: "Test".into(),
            model: "Test".into(),
            type_: "Moving Head".into(),
            channels: vec![
                Channel {
                    name: "Pan".into(),
                    preset: Some("PositionPan".into()),
                    group: None,
                    capabilities: vec![],
                },
                Channel {
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
            remote_id: None,
            uid: None,
            venue_id: 1,
            universe: 1,
            address: 1,
            num_channels: 2,
            manufacturer: "Test".into(),
            model: "Test".into(),
            mode_name: "TestMode".into(),
            fixture_path: "Test/Test.qxf".into(),
            label: None,
            pos_x: 0.0,
            pos_y: 0.0,
            pos_z: 0.0,
            rot_x: 0.0,
            rot_y: 0.0,
            rot_z: 0.0,
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
        let state = UniverseState { primitives };

        let buffers = generate_dmx(&state, &fixtures, &definitions, Some(&prev_map), 1.0);
        let buf = buffers.get(&1).unwrap();
        assert_eq!(buf[0], 123);
        assert_eq!(buf[1], 45);
    }
}
