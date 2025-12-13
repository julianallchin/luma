use std::collections::HashMap;
use std::net::UdpSocket;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager};

use crate::fixtures::engine;
use crate::fixtures::models::{FixtureDefinition, PatchedFixture};
use crate::fixtures::parser::parse_definition;
use crate::models::universe::UniverseState;
use crate::settings::AppSettings;

const ARTNET_PORT: u16 = 6454;
const HEADER: &[u8] = b"Art-Net\0";

#[derive(Clone, serde::Serialize)]
pub struct ArtNetNode {
    pub ip: String,
    pub name: String,
    pub long_name: String,
    pub port_address: u16, // Net/Subnet
    pub last_seen: u64,
}

pub struct ArtNetManager {
    inner: Arc<Mutex<ArtNetInner>>,
    discovery_handle: Arc<Mutex<Option<std::thread::JoinHandle<()>>>>,
}

struct ArtNetInner {
    socket: Option<UdpSocket>,
    sequence: u8,
    settings: AppSettings,
    patched_fixtures: Vec<PatchedFixture>,
    fixture_definitions: HashMap<String, FixtureDefinition>,
    fixtures_root: PathBuf,
    discovered_nodes: HashMap<String, ArtNetNode>,
    discovery_running: bool,
}

impl ArtNetManager {
    pub fn new(app: AppHandle) -> Self {
        let resource_path = app
            .path()
            .resource_dir()
            .map(|p| p.join("resources/fixtures/2511260420"))
            .unwrap_or_else(|_| PathBuf::from("resources/fixtures/2511260420"));

        let fixtures_root = if resource_path.exists() {
            resource_path
        } else {
            let cwd = std::env::current_dir().unwrap_or_default();
            let dev_path = cwd.join("../resources/fixtures/2511260420");
            if dev_path.exists() {
                dev_path
            } else {
                cwd.join("resources/fixtures/2511260420")
            }
        };

        let inner = Arc::new(Mutex::new(ArtNetInner {
            socket: None,
            sequence: 0,
            settings: AppSettings::default(),
            patched_fixtures: Vec::new(),
            fixture_definitions: HashMap::new(),
            fixtures_root,
            discovered_nodes: HashMap::new(),
            discovery_running: false,
        }));

        // Load settings asynchronously
        let inner_clone = inner.clone();
        let app_clone = app.clone();
        tauri::async_runtime::spawn(async move {
            if let Ok(settings) = crate::settings::get_all_settings(&app_clone).await {
                let mut guard = inner_clone.lock().unwrap();
                guard.settings = settings;
                drop(guard);
                Self::rebind(&inner_clone);
            }
        });

        Self {
            inner,
            discovery_handle: Arc::new(Mutex::new(None)),
        }
    }

    fn rebind(inner: &Arc<Mutex<ArtNetInner>>) {
        let mut guard = inner.lock().unwrap();

        // Close existing socket (drop it)
        guard.socket = None;

        println!(
            "[ArtNet] Rebind: Enabled={}, Discovery={}",
            guard.settings.artnet_enabled, guard.discovery_running
        );

        // Bind if enabled OR if discovery is running
        if !guard.settings.artnet_enabled && !guard.discovery_running {
            println!("[ArtNet] Rebind skipped (disabled and not discovering)");
            return;
        }

        let mut bind_ip = if guard.settings.artnet_interface.is_empty()
            || guard.settings.artnet_interface == "0.0.0.0"
        {
            "0.0.0.0".to_string()
        } else {
            guard.settings.artnet_interface.clone()
        };

        // If discovering and IP is generic, try to find a real one to ensure correct interface usage
        if guard.discovery_running && bind_ip == "0.0.0.0" {
            if let Ok(dummy) = UdpSocket::bind("0.0.0.0:0") {
                if dummy.connect("8.8.8.8:80").is_ok() {
                    if let Ok(local) = dummy.local_addr() {
                        let detected = local.ip().to_string();
                        println!(
                            "[ArtNet] Auto-detected local IP for discovery: {}",
                            detected
                        );
                        bind_ip = detected;
                    }
                }
            }
        }

        // Try binding to port 6454
        let addr = format!("{}:{}", bind_ip, ARTNET_PORT);
        match UdpSocket::bind(&addr) {
            Ok(s) => {
                let _ = s.set_broadcast(true);
                let _ = s.set_read_timeout(Some(Duration::from_millis(100)));
                println!("[ArtNet] Bound to {}", addr);
                guard.socket = Some(s);
            }
            Err(e) => {
                eprintln!("[ArtNet] Failed to bind to {}: {}", addr, e);
                // If specific bind failed, try fallback to 0.0.0.0 if we weren't already there
                if bind_ip != "0.0.0.0" {
                    println!("[ArtNet] Retrying with 0.0.0.0...");
                    let addr_any = format!("0.0.0.0:{}", ARTNET_PORT);
                    if let Ok(s) = UdpSocket::bind(&addr_any) {
                        let _ = s.set_broadcast(true);
                        let _ = s.set_read_timeout(Some(Duration::from_millis(100)));
                        println!("[ArtNet] Bound to {}", addr_any);
                        guard.socket = Some(s);
                    } else {
                        eprintln!("[ArtNet] Failed fallback bind.");
                    }
                }
            }
        }
    }

    pub fn update_patch(&self, fixtures: Vec<PatchedFixture>) {
        let mut guard = self.inner.lock().unwrap();
        guard.patched_fixtures = fixtures;

        // Load missing definitions
        let paths_to_load: Vec<String> = guard
            .patched_fixtures
            .iter()
            .map(|f| f.fixture_path.clone())
            .filter(|p| !guard.fixture_definitions.contains_key(p))
            .collect();

        let root = guard.fixtures_root.clone();
        for path_str in paths_to_load {
            let path = root.join(&path_str);
            if let Ok(def) = parse_definition(&path) {
                guard.fixture_definitions.insert(path_str, def);
            }
        }
    }

    pub fn broadcast(&self, state: &UniverseState) {
        let mut guard = self.inner.lock().unwrap();
        if !guard.settings.artnet_enabled {
            return;
        }
        if guard.socket.is_none() {
            return;
        }

        let universe_buffers =
            engine::generate_dmx(state, &guard.patched_fixtures, &guard.fixture_definitions);
        if universe_buffers.is_empty() {
            return;
        }

        let sequence = guard.sequence;
        guard.sequence = guard.sequence.wrapping_add(1);

        let socket = guard.socket.as_ref().unwrap();
        let broadcast_target = format!("255.255.255.255:{}", ARTNET_PORT);

        let unicast_target = if !guard.settings.artnet_unicast_ip.is_empty() {
            Some(format!(
                "{}:{}",
                guard.settings.artnet_unicast_ip, ARTNET_PORT
            ))
        } else {
            None
        };

        let should_broadcast = guard.settings.artnet_broadcast;

        for (univ_idx, data) in universe_buffers {
            // Apply Net/Subnet offset
            // Physical Universe = (Net << 8) | (Subnet << 4) | (Universe & 0xF)
            // But ArtNet 3/4 uses 15-bit Port-Address directly.
            // Let's assume settings provide Net (0-127) and Subnet (0-15).
            // And `univ_idx` is the universe index (0-15).

            let net = guard.settings.artnet_net;
            let subnet = guard.settings.artnet_subnet;

            // Port Address: Bits 14-8 = Net, 7-4 = SubNet, 3-0 = Universe
            let port_address =
                ((net as u16) << 8) | ((subnet as u16) << 4) | (univ_idx as u16 & 0xF);

            let packet = build_artdmx_packet(sequence, port_address, &data);

            if let Some(target) = &unicast_target {
                let _ = socket.send_to(&packet, target);
            }

            if should_broadcast || unicast_target.is_none() {
                let _ = socket.send_to(&packet, &broadcast_target);
            }
        }
    }
}

pub async fn reload_settings(app: &AppHandle) -> Result<(), String> {
    let manager = app.state::<ArtNetManager>();
    let settings = crate::settings::get_all_settings(app).await?;

    let mut guard = manager.inner.lock().unwrap();
    guard.settings = settings.clone();
    if !settings.artnet_enabled {
        // If Art-Net output is disabled, ensure discovery stops too so
        // rebind will close the socket and no packets are sent.
        guard.discovery_running = false;
    }
    drop(guard);

    ArtNetManager::rebind(&manager.inner);

    Ok(())
}

fn build_artdmx_packet(sequence: u8, port_address: u16, data: &[u8; 512]) -> Vec<u8> {
    let mut packet = Vec::with_capacity(18 + 512);
    packet.extend_from_slice(HEADER);
    packet.push(0x00);
    packet.push(0x50); // OpOutput (0x5000) -> 00 50 Little Endian?? No, OpCodes are Little Endian in spec, so 0x5000 is 0x00 0x50.
                       // Wait. Spec says "OpCode ... Low Byte first". 0x5000 -> 0x00, 0x50. Correct.
    packet.push(0x00);
    packet.push(0x0E); // ProtoVer 14 -> 00 0E (Big Endian)
    packet.push(sequence);
    packet.push(0x00); // Physical
    packet.push((port_address & 0xFF) as u8); // SubUni
    packet.push(((port_address >> 8) & 0x7F) as u8); // Net
    packet.push(0x02);
    packet.push(0x00); // Length 512 (Big Endian)
    packet.extend_from_slice(data);
    packet
}

fn build_artpoll_packet() -> Vec<u8> {
    let mut packet = Vec::with_capacity(14);
    packet.extend_from_slice(HEADER);
    packet.push(0x00);
    packet.push(0x20); // OpPoll (0x2000) -> 00 20
    packet.push(0x00);
    packet.push(0x0E); // Ver 14
    packet.push(0x00); // TalkToMe: Auto
    packet.push(0x00); // Priority
    packet
}

// -- Tauri Commands --

#[tauri::command]
pub fn start_discovery(app: AppHandle) {
    println!("[ArtNet] start_discovery called");
    let manager = app.state::<ArtNetManager>();
    let inner = manager.inner.clone();

    let mut guard = inner.lock().unwrap();
    if guard.discovery_running {
        println!("[ArtNet] Discovery already running");
        return;
    }

    // Set discovery running TRUE so rebind knows we need a socket
    guard.discovery_running = true;

    if guard.socket.is_none() {
        // Try to init if not ready
        println!("[ArtNet] Socket not ready, attempting rebind for discovery...");
        drop(guard); // Unlock to allow rebind to lock
        ArtNetManager::rebind(&inner);
        guard = inner.lock().unwrap(); // Relock

        if guard.socket.is_none() {
            eprintln!("[ArtNet] Cannot start discovery: No socket.");
            guard.discovery_running = false; // Reset flag since we failed
            return;
        }
    }

    // We need a socket clone for the thread
    let socket = guard.socket.as_ref().unwrap().try_clone().ok();
    if socket.is_none() {
        guard.discovery_running = false;
        return;
    }
    let socket = socket.unwrap();

    drop(guard); // Unlock before spawning

    let inner_thread = inner.clone();

    std::thread::spawn(move || {
        let mut last_poll = Instant::now();
        let poll_interval = Duration::from_secs(3);
        let mut buf = [0u8; 1024];

        // Try to determine directed broadcast address
        let mut directed_broadcasts = Vec::new();

        // 1. Generic Limited Broadcast
        directed_broadcasts.push(format!("255.255.255.255:{}", ARTNET_PORT));

        // 2. Try to find local IP to guess directed broadcast
        if let Ok(dummy_socket) = UdpSocket::bind("0.0.0.0:0") {
            if dummy_socket.connect("8.8.8.8:80").is_ok() {
                if let Ok(local_addr) = dummy_socket.local_addr() {
                    let ip = local_addr.ip();
                    if let std::net::IpAddr::V4(ipv4) = ip {
                        let octets = ipv4.octets();
                        // Assume /24 for home networks: x.x.x.255
                        let broadcast_ip = format!("{}.{}.{}.255", octets[0], octets[1], octets[2]);
                        println!(
                            "[ArtNet] Discovery: Guessed directed broadcast {}",
                            broadcast_ip
                        );
                        directed_broadcasts.push(format!("{}:{}", broadcast_ip, ARTNET_PORT));
                    }
                }
            }
        }

        // Send initial poll
        let poll_pkt = build_artpoll_packet();
        println!(
            "[ArtNet] Discovery thread: Sending initial ArtPoll to {:?}",
            directed_broadcasts
        );
        for target in &directed_broadcasts {
            let _ = socket.send_to(&poll_pkt, target);
        }

        loop {
            // Check if we should stop
            {
                let guard = inner_thread.lock().unwrap();
                if !guard.discovery_running || guard.socket.is_none() {
                    break;
                }
            }

            // Send Poll periodically
            if last_poll.elapsed() >= poll_interval {
                println!("[ArtNet] Discovery thread: Sending ArtPoll...");
                for target in &directed_broadcasts {
                    let _ = socket.send_to(&poll_pkt, target);
                }
                last_poll = Instant::now();
            }

            // Listen for replies
            // Socket has timeout
            match socket.recv_from(&mut buf) {
                Ok((size, src)) => {
                    println!("[ArtNet] Received {} bytes from {}", size, src);
                    if size > 10 {
                        if &buf[0..8] == HEADER {
                            let opcode = (buf[9] as u16) << 8 | (buf[8] as u16);
                            println!("[ArtNet] Packet OpCode: 0x{:04X}", opcode);

                            if opcode == 0x2100 {
                                // OpPollReply
                                // Parse
                                let ip = src.ip().to_string();

                                // Extract Names
                                // Short Name: offset 26, 18 bytes
                                let short_name_bytes = &buf[26..26 + 18];
                                let short_name = String::from_utf8_lossy(short_name_bytes)
                                    .trim_matches(char::from(0))
                                    .to_string();

                                // Long Name: offset 44, 64 bytes
                                let long_name_bytes = &buf[44..44 + 64];
                                let long_name = String::from_utf8_lossy(long_name_bytes)
                                    .trim_matches(char::from(0))
                                    .to_string();

                                // Port Addr
                                let net = buf[18] as u16;
                                let sub = buf[19] as u16;
                                let port_addr = (net << 8) | (sub << 4);

                                println!(
                                    "[ArtNet] Found Node: {} ({}) at {}",
                                    short_name, long_name, ip
                                );

                                let node = ArtNetNode {
                                    ip: ip.clone(),
                                    name: short_name,
                                    long_name,
                                    port_address: port_addr,
                                    last_seen: SystemTime::now()
                                        .duration_since(UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_secs(),
                                };

                                let mut guard = inner_thread.lock().unwrap();
                                guard.discovered_nodes.insert(ip, node);
                            }
                        } else {
                            println!("[ArtNet] Invalid Header: {:?}", &buf[0..8]);
                        }
                    }
                }
                Err(e) => {
                    // Timeout is expected, don't log it to avoid spam
                    if e.kind() != std::io::ErrorKind::WouldBlock
                        && e.kind() != std::io::ErrorKind::TimedOut
                    {
                        eprintln!("[ArtNet] Recv error: {}", e);
                    }
                }
            }
        }
    });
}

#[tauri::command]
pub fn stop_discovery(app: AppHandle) {
    let manager = app.state::<ArtNetManager>();
    let inner = manager.inner.clone();

    let mut guard = inner.lock().unwrap();
    guard.discovery_running = false;
    drop(guard);

    // Check if we should close the socket (rebind handles this logic)
    ArtNetManager::rebind(&inner);
}

#[tauri::command]
pub fn get_discovered_nodes(state: tauri::State<ArtNetManager>) -> Vec<ArtNetNode> {
    let guard = state.inner.lock().unwrap();
    guard.discovered_nodes.values().cloned().collect()
}
