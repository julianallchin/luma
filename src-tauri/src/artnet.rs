use std::collections::HashMap;
use std::net::UdpSocket;
use std::sync::{Arc, Mutex};
use std::path::PathBuf;
use tauri::{AppHandle, Manager};

use crate::models::universe::UniverseState;
use crate::fixtures::models::{PatchedFixture, FixtureDefinition};
use crate::fixtures::parser::parse_definition;

const ARTNET_PORT: u16 = 6454;
const DEFAULT_INTERFACE_IP: &str = "10.0.0.3";
const TARGET_IP: &str = "10.201.6.100"; // 10.201.006.100

pub struct ArtNetManager {
    inner: Arc<Mutex<ArtNetInner>>,
}

struct ArtNetInner {
    socket: Option<UdpSocket>,
    sequence: u8,
    patched_fixtures: Vec<PatchedFixture>,
    fixture_definitions: HashMap<String, FixtureDefinition>, // Keyed by fixture_path
    fixtures_root: PathBuf,
}

impl ArtNetManager {
    pub fn new(app: &AppHandle) -> Self {
        let resource_path = app
            .path()
            .resource_dir()
            .map(|p| p.join("resources/fixtures/2511260420"))
            .unwrap_or_else(|_| PathBuf::from("resources/fixtures/2511260420"));

        let fixtures_root = if resource_path.exists() {
            resource_path
        } else {
            // Dev fallback
            let cwd = std::env::current_dir().unwrap_or_default();
            let dev_path = cwd.join("../resources/fixtures/2511260420");
            if dev_path.exists() {
                dev_path
            } else {
                cwd.join("resources/fixtures/2511260420")
            }
        };

        let socket = UdpSocket::bind(format!("{}:0", DEFAULT_INTERFACE_IP))
            .or_else(|_| UdpSocket::bind("0.0.0.0:0"))
            .ok();
            
        if let Some(s) = &socket {
            if let Ok(addr) = s.local_addr() {
                println!("[ArtNet] Bound to local address: {:?}", addr);
            }
            let _ = s.set_broadcast(true);
            println!("[ArtNet] Socket created and broadcast enabled.");
        } else {
            println!("[ArtNet] FAILED to create socket. ArtNet will not work.");
        }

        Self {
            inner: Arc::new(Mutex::new(ArtNetInner {
                socket,
                sequence: 0,
                patched_fixtures: Vec::new(),
                fixture_definitions: HashMap::new(),
                fixtures_root,
            })),
        }
    }

    pub fn update_patch(&self, fixtures: Vec<PatchedFixture>) {
        let mut guard = self.inner.lock().unwrap();
        guard.patched_fixtures = fixtures;
        
        println!("[ArtNet] Updating patch with {} fixtures.", guard.patched_fixtures.len());
        
        // Collect paths to load first to avoid borrow issues
        let paths_to_load: Vec<String> = guard.patched_fixtures.iter()
            .map(|f| f.fixture_path.clone())
            .filter(|p| !guard.fixture_definitions.contains_key(p))
            .collect();
            
        if !paths_to_load.is_empty() {
            println!("[ArtNet] Loading {} new fixture definitions.", paths_to_load.len());
        }
            
        // Load definitions for new fixtures
        let root = guard.fixtures_root.clone();
        for path_str in paths_to_load {
            let path = root.join(&path_str);
            if let Ok(def) = parse_definition(&path) {
                println!("[ArtNet] Loaded definition for {}", path_str);
                guard.fixture_definitions.insert(path_str, def);
            } else {
                eprintln!("[ArtNet] Failed to load fixture definition: {:?}", path);
            }
        }
    }

    pub fn broadcast(&self, state: &UniverseState) {
        let mut guard = self.inner.lock().unwrap();
        if guard.socket.is_none() {
            return;
        }

        // For now, assume 1 universe (Universe 0 or 1)
        // The user mentioned converting universe state to DMX
        // We'll construct a buffer for each universe present in patched fixtures
        
        let mut universe_buffers: HashMap<i64, [u8; 512]> = HashMap::new();
        let mut mapped_channels = 0;

        for fixture in &guard.patched_fixtures {
            let def = match guard.fixture_definitions.get(&fixture.fixture_path) {
                Some(d) => d,
                None => continue,
            };

            // Find active mode
            let mode = def.modes.iter().find(|m| m.name == fixture.mode_name);
            if let Some(mode) = mode {
                // Ensure buffer exists for this universe
                let buffer = universe_buffers.entry(fixture.universe).or_insert([0; 512]);
                
                // Build map of channel index -> head index
                let mut channel_to_head: HashMap<u32, usize> = HashMap::new();
                for (head_idx, head) in mode.heads.iter().enumerate() {
                    for &channel_idx in &head.channels {
                        channel_to_head.insert(channel_idx, head_idx);
                    }
                }

                // Iterate channels in mode
                for (i, mode_channel) in mode.channels.iter().enumerate() {
                    let dmx_address = (fixture.address - 1) as usize + i;
                    if dmx_address >= 512 { continue; }
                    
                    // Determine Head and Primitive ID
                    let head_idx = channel_to_head.get(&(i as u32));
                    
                    // 1. Try specific head ID: "fixture_id:head_index"
                    // 2. Fallback to main fixture ID: "fixture_id"
                    let prim_state = if let Some(h_idx) = head_idx {
                        let head_id = format!("{}:{}", fixture.id, h_idx);
                        state.primitives.get(&head_id).or_else(|| state.primitives.get(&fixture.id))
                    } else {
                        state.primitives.get(&fixture.id)
                    };

                    if let Some(prim_state) = prim_state {
                        // Find channel definition in fixture
                        if let Some(chan_def) = def.channels.iter().find(|c| c.name == mode_channel.name) {
                            let mut value: u8 = 0;

                            // Mapping Logic
                            if let Some(group) = &chan_def.group {
                                match group.value.as_str() {
                                    "Intensity" => {
                                        value = (prim_state.dimmer * 255.0) as u8;
                                    }
                                    "Colour" => {
                                        let name_lower = chan_def.name.to_lowercase();
                                        if name_lower.contains("red") {
                                            value = (prim_state.color[0] * 255.0) as u8;
                                        } else if name_lower.contains("green") {
                                            value = (prim_state.color[1] * 255.0) as u8;
                                        } else if name_lower.contains("blue") {
                                            value = (prim_state.color[2] * 255.0) as u8;
                                        } else if name_lower.contains("white") {
                                            value = 0;
                                        }
                                    }
                                    "Shutter" => {
                                        if prim_state.strobe > 0.0 {
                                             value = (prim_state.strobe * 255.0) as u8;
                                        } else {
                                            value = 0; 
                                        }
                                    }
                                    _ => {}
                                }
                            } else {
                                // Fallback by name
                                let name_lower = chan_def.name.to_lowercase();
                                if name_lower.contains("red") {
                                    value = (prim_state.color[0] * 255.0) as u8;
                                } else if name_lower.contains("green") {
                                    value = (prim_state.color[1] * 255.0) as u8;
                                } else if name_lower.contains("blue") {
                                    value = (prim_state.color[2] * 255.0) as u8;
                                } else if name_lower.contains("dimmer") || name_lower.contains("intensity") {
                                    value = (prim_state.dimmer * 255.0) as u8;
                                } else if name_lower.contains("strobe") || name_lower.contains("shutter") {
                                    if prim_state.strobe > 0.0 {
                                        // Map to range 10-255 to avoid potential "Closed" zones at very low values
                                        value = ((prim_state.strobe * 245.0) + 10.0) as u8;
                                    } else {
                                        value = 0;
                                    }
                                }
                            }
                            
                            buffer[dmx_address] = value;
                            mapped_channels += 1;
                        }
                    }
                }
            }
        }
        
        // Send ArtDmx packets
        let sequence = guard.sequence;
        guard.sequence = guard.sequence.wrapping_add(1);

        for (universe, data) in universe_buffers {
            // Construct 15-bit Port-Address from universe index
            // We assume fixture.universe is the absolute ArtNet universe (0-32767)
            let packet = build_artdmx_packet(sequence, universe as u16, &data);
            
            // 1. Unicast to Target
            let target = format!("{}:{}", TARGET_IP, ARTNET_PORT);
            if let Err(e) = guard.socket.as_ref().unwrap().send_to(&packet, &target) {
                eprintln!("[ArtNet] Failed to send Unicast to {}: {}", target, e);
            }

            // 2. Broadcast (Fallback for subnet mismatch)
            let broadcast_target = format!("255.255.255.255:{}", ARTNET_PORT);
             if let Err(_e) = guard.socket.as_ref().unwrap().send_to(&packet, &broadcast_target) {
                // Don't spam error if broadcast fails (e.g. permission)
            }

            // Log every 60th frame (approx 1 sec) to confirm transmission
            if sequence % 60 == 0 {
                // Clean log
                println!("[ArtNet] Transmitting Universe {}: {} channels mapped", universe, mapped_channels);
            }
        }
    }
}

fn build_artdmx_packet(sequence: u8, universe_address: u16, data: &[u8; 512]) -> Vec<u8> {
    let mut packet = Vec::with_capacity(18 + 512);
    
    // ID "Art-Net\0"
    packet.extend_from_slice(b"Art-Net\0");
    
    // OpCode ArtDmx (0x5000) - Little Endian: 0x00 0x50
    packet.push(0x00);
    packet.push(0x50);
    
    // Protocol Version (14) - Big Endian: 0x00 0x0E
    packet.push(0x00);
    packet.push(0x0E);
    
    // Sequence
    packet.push(sequence);
    
    // Physical (0)
    packet.push(0x00);
    
    // Port-Address (15 bit)
    // Byte 14: SubUni (Low 8 bits)
    // Byte 15: Net (High 7 bits)
    packet.push((universe_address & 0xFF) as u8);
    packet.push(((universe_address >> 8) & 0x7F) as u8);
    
    // Length (512) - Big Endian: 0x02 0x00
    packet.push(0x02);
    packet.push(0x00);
    
    // Data
    packet.extend_from_slice(data);
    
    packet
}
