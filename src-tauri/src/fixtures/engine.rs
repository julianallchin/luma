use crate::fixtures::models::{ChannelColour, ChannelType, FixtureDefinition, PatchedFixture};
use crate::models::universe::{PrimitiveState, UniverseState};
use std::collections::HashMap;

pub fn generate_dmx(
    state: &UniverseState,
    fixtures: &[PatchedFixture],
    definitions: &HashMap<String, FixtureDefinition>,
    previous_universe_buffers: Option<&HashMap<i64, [u8; 512]>>,
) -> HashMap<i64, [u8; 512]> {
    let mut buffers: HashMap<i64, [u8; 512]> = HashMap::new();

    for fixture in fixtures {
        let def = match definitions.get(&fixture.fixture_path) {
            Some(d) => d,
            None => continue,
        };

        let mode = match def.modes.iter().find(|m| m.name == fixture.mode_name) {
            Some(m) => m,
            None => continue,
        };

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

            match map_value(channel, prim) {
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

fn map_value(channel: &crate::fixtures::models::Channel, state: &PrimitiveState) -> MapAction {
    let ch_type = channel.get_type();

    match ch_type {
        ChannelType::Intensity => {
            // Check if it's a specific color intensity (some fixtures have "Red" channel type as Intensity)
            // But get_type() usually separates Colour from Intensity.
            // However, QLC+ might tag Red as IntensityRed preset.
            // My get_type logic: IntensityRed -> Intensity.
            // So I need to check colour too.

            MapAction::Set(match channel.get_colour() {
                ChannelColour::Red => (state.color[0] * 255.0) as u8,
                ChannelColour::Green => (state.color[1] * 255.0) as u8,
                ChannelColour::Blue => (state.color[2] * 255.0) as u8,
                ChannelColour::White => 0, // TODO: Add white support to PrimitiveState
                ChannelColour::Amber => 0,
                ChannelColour::UV => 0,
                ChannelColour::None => (state.dimmer * 255.0) as u8, // Master Dimmer
                _ => 0,
            })
        }
        ChannelType::Colour => {
            // Colour group can be:
            // - RGB/CMY/etc mixer channels (rarely tagged as Colour in QXF; often Intensity*)
            // - Color wheel / color macro channel with capabilities describing colors
            match channel.get_colour() {
                ChannelColour::Red => MapAction::Set((state.color[0] * 255.0) as u8),
                ChannelColour::Green => MapAction::Set((state.color[1] * 255.0) as u8),
                ChannelColour::Blue => MapAction::Set((state.color[2] * 255.0) as u8),
                ChannelColour::White => MapAction::Set(0),
                ChannelColour::Amber => MapAction::Set(0),
                ChannelColour::UV => MapAction::Set(0),
                ChannelColour::None => {
                    if is_black(state.color) {
                        MapAction::Hold
                    } else {
                        MapAction::Set(map_nearest_color_capability(channel, state.color).unwrap_or(0))
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
        _ => MapAction::Set(0),
    }
}

fn map_nearest_color_capability(
    channel: &crate::fixtures::models::Channel,
    desired_rgb: [f32; 3],
) -> Option<u8> {
    let mut best: Option<(f32, u8)> = None;

    for cap in &channel.capabilities {
        let Some(rgb) = capability_rgb(cap) else { continue };
        let d = color_distance(rgb, desired_rgb);
        let value = cap.min;

        match best {
            None => best = Some((d, value)),
            Some((best_d, _)) if d < best_d => best = Some((d, value)),
            _ => {}
        }
    }

    best.map(|(_, v)| v)
}

fn capability_rgb(cap: &crate::fixtures::models::Capability) -> Option<[f32; 3]> {
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

fn is_black(rgb: [f32; 3]) -> bool {
    rgb[0] <= 0.0001 && rgb[1] <= 0.0001 && rgb[2] <= 0.0001
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::models::{Channel, Mode, ModeChannel};
    use crate::models::universe::{PrimitiveState, UniverseState};

    fn prim(dimmer: f32, r: f32, g: f32, b: f32, strobe: f32) -> PrimitiveState {
        PrimitiveState {
            dimmer,
            color: [r, g, b],
            strobe,
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
                heads: vec![crate::fixtures::models::Head {
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

        let buffers = generate_dmx(&state, &fixtures, &definitions, None);
        let buf = buffers.get(&1).expect("universe buffer");

        // Pan is channel number 0 => DMX address 0 (0-based) and should be 0 by default.
        assert_eq!(buf[0], 0);
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

        let buffers = generate_dmx(&state, &fixtures, &definitions, None);
        let buf = buffers.get(&1).expect("universe buffer");

        // Start address 49 => 0-based 48. Channel number 5 => index 53 (DMX channel 54).
        assert_eq!(buf[53], 255);
     }

    #[test]
    fn maps_color_wheel_to_nearest_capability() {
        let channel = Channel {
            name: "Colors".into(),
            preset: None,
            group: Some(crate::fixtures::models::Group {
                byte: 0,
                value: "Colour".into(),
            }),
            capabilities: vec![
                crate::fixtures::models::Capability {
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
                crate::fixtures::models::Capability {
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
                crate::fixtures::models::Capability {
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
        assert_eq!(map_value(&channel, &state), MapAction::Set(10));

        let state = prim(1.0, 0.05, 0.95, 0.05, 0.0);
        assert_eq!(map_value(&channel, &state), MapAction::Set(20));
    }

    #[test]
    fn holds_wheel_value_when_color_is_black() {
        let channel = Channel {
            name: "Colors".into(),
            preset: None,
            group: Some(crate::fixtures::models::Group {
                byte: 0,
                value: "Colour".into(),
            }),
            capabilities: vec![
                crate::fixtures::models::Capability {
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
                crate::fixtures::models::Capability {
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

        let buffers1 = generate_dmx(&state, &fixtures, &definitions, None);
        let prev = buffers1.get(&1).copied().unwrap();
        assert_eq!(prev[0], 10);

        // Second frame sends black -> hold previous wheel value
        let mut primitives = HashMap::new();
        primitives.insert("fx".into(), prim(1.0, 0.0, 0.0, 0.0, 0.0));
        let state2 = UniverseState { primitives };

        let mut prev_map = HashMap::new();
        prev_map.insert(1i64, prev);
        let buffers2 = generate_dmx(&state2, &fixtures, &definitions, Some(&prev_map));
        let buf2 = buffers2.get(&1).unwrap();
        assert_eq!(buf2[0], 10);
    }
}
