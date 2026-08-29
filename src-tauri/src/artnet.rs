use std::collections::HashMap;
use std::net::UdpSocket;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager};

use crate::fixtures::engine;
use crate::fixtures::parser::parse_definition;
use crate::models::fixtures::{FixtureDefinition, PatchedFixture};
use crate::models::patch::UniverseOutput;
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
    last_universe_buffers: HashMap<i64, [u8; 512]>,
    fixtures_root: PathBuf,
    discovered_nodes: HashMap<String, ArtNetNode>,
    /// Which node each universe goes to, by universe. The table that replaced
    /// the port-address arithmetic; a universe missing from it falls back to
    /// that arithmetic, loudly.
    outputs: HashMap<i64, UniverseOutput>,
    /// Universes already warned about, so the fallback says its piece once
    /// rather than forty-four times a second.
    unbound_warned: std::collections::HashSet<i64>,
    discovery_running: bool,
    // Monotonic epoch used to derive frame_time_secs for time-varying effects
    // (currently the square-wave strobe fallback for fixtures without a shutter).
    start: Instant,
}

impl ArtNetManager {
    pub fn new(app: AppHandle) -> Self {
        let fixtures_root = crate::services::fixtures::resolve_fixtures_root(&app)
            .unwrap_or_else(|_| PathBuf::from("resources/fixtures/2511260420"));

        let inner = Arc::new(Mutex::new(ArtNetInner {
            socket: None,
            sequence: 0,
            settings: AppSettings::default(),
            patched_fixtures: Vec::new(),
            fixture_definitions: HashMap::new(),
            last_universe_buffers: HashMap::new(),
            fixtures_root,
            discovered_nodes: HashMap::new(),
            outputs: HashMap::new(),
            unbound_warned: std::collections::HashSet::new(),
            discovery_running: false,
            start: Instant::now(),
        }));

        // Load settings asynchronously
        let inner_clone = inner.clone();
        let pool = app.state::<crate::database::Db>().inner().0.clone();
        tauri::async_runtime::spawn(async move {
            if let Ok(settings) = crate::settings::load_settings(&pool).await {
                let mut guard = inner_clone.lock().unwrap();
                guard.settings = settings;
                drop(guard);
                Self::rebind(&inner_clone);
            }
            if let Ok(outputs) = crate::database::local::outputs::list(&pool).await {
                inner_clone.lock().unwrap().outputs =
                    outputs.into_iter().map(|o| (o.universe, o)).collect();
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

    /// Replace the universe-to-node table. Called after every bind or unbind,
    /// so the sender never reads the database on a frame.
    pub fn set_outputs(&self, outputs: Vec<UniverseOutput>) {
        let mut guard = self.inner.lock().unwrap();
        guard.outputs = outputs.into_iter().map(|o| (o.universe, o)).collect();
        // A universe that has just been unbound deserves to be told about
        // again; a bound one will never take the fallback branch anyway.
        guard.unbound_warned.clear();
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
        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(e) => {
                eprintln!("[ArtNet] mutex recovered from poison");
                e.into_inner()
            }
        };
        if !guard.settings.artnet_enabled {
            return;
        }
        if guard.socket.is_none() {
            return;
        }

        let frame_time_secs = guard.start.elapsed().as_secs_f64();
        let universe_buffers = engine::generate_dmx(
            state,
            &guard.patched_fixtures,
            &guard.fixture_definitions,
            Some(&guard.last_universe_buffers),
            (guard.settings.max_dimmer as f32) / 100.0,
            frame_time_secs,
        );
        if universe_buffers.is_empty() {
            return;
        }
        guard.last_universe_buffers = universe_buffers.clone();

        let sequence = guard.sequence;
        guard.sequence = guard.sequence.wrapping_add(1);

        // Resolve every universe's destination *before* the socket is
        // borrowed. A bound universe carries the port address its node
        // announced and goes straight to that node; an unbound one falls back
        // to the old `(net << 8) | (subnet << 4) | (universe & 0xF)`
        // arithmetic, which masks the universe to four bits — so universe 17
        // lands on universe 1 — and says so once rather than aliasing in
        // silence.
        let net = guard.settings.artnet_net;
        let subnet = guard.settings.artnet_subnet;
        let routes: Vec<(i64, u16, Option<String>)> = universe_buffers
            .keys()
            .copied()
            .map(|universe| match guard.outputs.get(&universe) {
                Some(output) => (
                    universe,
                    output.port_address as u16,
                    Some(format!("{}:{}", output.node_ip, output.node_port)),
                ),
                None => (
                    universe,
                    ((net as u16) << 8) | ((subnet as u16) << 4) | (universe as u16 & 0xF),
                    None,
                ),
            })
            .collect();
        for (universe, _, node) in &routes {
            if node.is_none() && guard.unbound_warned.insert(*universe) {
                log::warn!(
                    "[ArtNet] universe {universe} is bound to no node; falling back to \
                     net/subnet arithmetic, which aliases universes above 15 onto 0-15"
                );
            }
        }

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

        for (universe, port_address, node) in &routes {
            let Some(data) = universe_buffers.get(universe) else {
                continue;
            };
            let packet = build_artdmx_packet(sequence, *port_address, data);

            match node {
                // A binding is a destination, so it neither broadcasts nor
                // consults the global unicast setting: the whole point of the
                // table is that two nodes can hold different universes.
                Some(node) => {
                    let _ = socket.send_to(&packet, node);
                }
                None => {
                    if let Some(target) = &unicast_target {
                        let _ = socket.send_to(&packet, target);
                    }
                    if should_broadcast || unicast_target.is_none() {
                        let _ = socket.send_to(&packet, &broadcast_target);
                    }
                }
            }
        }
    }
}

/// Re-read the persisted settings into `manager` and rebind its socket.
pub async fn reload_settings(
    manager: &ArtNetManager,
    pool: &sqlx::SqlitePool,
) -> Result<(), String> {
    let settings = crate::settings::load_settings(pool).await?;

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

// -- Discovery --

impl ArtNetManager {
    /// Begin broadcasting ArtPoll and collecting replies. Idempotent: a second
    /// call while discovery is running is a no-op.
    ///
    /// Cannot fail from the caller's side — a socket that will not bind is
    /// logged and leaves `discovered_nodes` empty, which is what the poller
    /// already handles.
    pub fn start_discovery(&self) {
        println!("[ArtNet] start_discovery called");
        let manager = self;
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

        let handle = std::thread::spawn(move || {
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
                            let broadcast_ip =
                                format!("{}.{}.{}.255", octets[0], octets[1], octets[2]);
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

        // Track the discovery thread so it can be joined on drop
        let mut handle_slot = manager.discovery_handle.lock().unwrap();
        // Join any previous handle if still around
        if let Some(h) = handle_slot.take() {
            let _ = h.join();
        }
        *handle_slot = Some(handle);
    }

    /// Ask the discovery thread to stop. Cooperative, not a join: it exits at
    /// its next loop check, up to one socket timeout later. Already-discovered
    /// nodes are kept — the next scan overwrites them by IP.
    pub fn stop_discovery(&self) {
        let inner = self.inner.clone();

        let mut guard = inner.lock().unwrap();
        guard.discovery_running = false;
        drop(guard);

        // Check if we should close the socket (rebind handles this logic)
        Self::rebind(&inner);
    }

    /// Every node seen since the manager was built. Iteration order is a
    /// `HashMap`'s — non-deterministic; `last_seen` is the only staleness
    /// signal, since entries are never evicted.
    pub fn discovered_nodes(&self) -> Vec<ArtNetNode> {
        let guard = self.inner.lock().unwrap();
        guard.discovered_nodes.values().cloned().collect()
    }
}

impl Drop for ArtNetManager {
    fn drop(&mut self) {
        // Signal stop and join discovery thread if running
        if let Some(inner) = Arc::get_mut(&mut self.inner) {
            if let Ok(mut guard) = inner.lock() {
                guard.discovery_running = false;
            }
        }
        if let Ok(mut handle_slot) = self.discovery_handle.lock() {
            if let Some(handle) = handle_slot.take() {
                let _ = handle.join();
            }
        }
    }
}
