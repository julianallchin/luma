use crate::fixtures::models::{ChannelColour, ChannelType, FixtureDefinition, PatchedFixture};
use crate::models::universe::{PrimitiveState, UniverseState};
use std::collections::HashMap;

pub fn generate_dmx(
    state: &UniverseState,
    fixtures: &[PatchedFixture],
    definitions: &HashMap<String, FixtureDefinition>,
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
            let head_idx = channel_to_head.get(&mode_channel.number);
            let head_prim = head_idx.and_then(|h_idx| {
                let head_id = format!("{}:{}", fixture.id, h_idx);
                state.primitives.get(&head_id)
            });

            // If a dimmer channel ends up in a <Head>, it still usually represents a
            // fixture-level master dimmer. Prefer fixture primitive dimmer in that case.
            let prim = match (head_prim, fixture_prim) {
                (Some(h), Some(f)) => {
                    let ch_type = channel.get_type();
                    let ch_colour = channel.get_colour();
                    if ch_type == ChannelType::Intensity && ch_colour == ChannelColour::None {
                        f
                    } else {
                        h
                    }
                }
                (Some(h), None) => h,
                (None, Some(f)) => f,
                (None, None) => continue,
            };

            buffer[dmx_address] = map_value(channel, prim);
        }
    }

    buffers
}

fn map_value(channel: &crate::fixtures::models::Channel, state: &PrimitiveState) -> u8 {
    let ch_type = channel.get_type();

    match ch_type {
        ChannelType::Intensity => {
            // Check if it's a specific color intensity (some fixtures have "Red" channel type as Intensity)
            // But get_type() usually separates Colour from Intensity.
            // However, QLC+ might tag Red as IntensityRed preset.
            // My get_type logic: IntensityRed -> Intensity.
            // So I need to check colour too.

            match channel.get_colour() {
                ChannelColour::Red => (state.color[0] * 255.0) as u8,
                ChannelColour::Green => (state.color[1] * 255.0) as u8,
                ChannelColour::Blue => (state.color[2] * 255.0) as u8,
                ChannelColour::White => 0, // TODO: Add white support to PrimitiveState
                ChannelColour::Amber => 0,
                ChannelColour::UV => 0,
                ChannelColour::None => (state.dimmer * 255.0) as u8, // Master Dimmer
                _ => 0,
            }
        }
        ChannelType::Colour => {
            // If type is Colour, it's likely a color wheel or fixed color channel, OR RGB channel if parsed that way.
            // My get_type maps IntensityRed -> Intensity.
            // What if it maps to Colour? (e.g. preset "ColorRed"?)
            match channel.get_colour() {
                ChannelColour::Red => (state.color[0] * 255.0) as u8,
                ChannelColour::Green => (state.color[1] * 255.0) as u8,
                ChannelColour::Blue => (state.color[2] * 255.0) as u8,
                _ => 0,
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
                    return ((state.strobe * 245.0) + 10.0) as u8;
                } else {
                    return 0; // Open/Closed? Usually 0 is open or closed depending on fixture.
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
                    return val.clamp(cap.min as f32, cap.max as f32) as u8;
                }

                // Fallback: if no specific strobe capability found, but we are in Shutter channel,
                // try to find "Strobe" string in any capability.
                // My is_strobe() helper does this.

                // If absolutely nothing found, return linear map?
                return ((state.strobe * 245.0) + 10.0) as u8;
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
                    return cap.min; // Return start of Open range
                }

                // Fallback for "Shutter" channel: 255 is often Open. 0 might be Closed.
                // But some fixtures 0 is Open.
                // We'll default to 0 if we can't find "Open".
                // Actually, if we defaulted Strobe>0 to linear, we imply 0 is off.
                return 0;
            }
        }
        _ => 0,
    }
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

        let buffers = generate_dmx(&state, &fixtures, &definitions);
        let buf = buffers.get(&1).expect("universe buffer");

        // Pan is channel number 0 => DMX address 0 (0-based) and should be 0 by default.
        assert_eq!(buf[0], 0);
        // Dimmer is channel number 1 => DMX address 1 and should come from fixture primitive (255).
        assert_eq!(buf[1], 255);
        // Red is channel number 2 => DMX address 2 and should come from head primitive (255).
        assert_eq!(buf[2], 255);
    }
}
