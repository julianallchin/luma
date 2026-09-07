pub mod metadata;
pub mod packets;
pub mod types;

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;

use serde::Serialize;
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::{interval, sleep, Duration};

use crate::metadata::{fetch_metadata, MetadataRequest, MetadataResult};
use crate::packets::{
    build_hello, build_keepalive, build_stage1, build_stage2, build_stage3, parse_keepalive,
    parse_position, parse_status,
};
use crate::types::*;

// ── Public discovery types ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct DiscoveredDevice {
    pub player: u8,
    pub name: String,
    pub ip: String,
}

/// Passively listen on port 50000 for CDJ keepalives for `timeout_ms` milliseconds.
/// Returns unique CDJs found, sorted by player number.
/// Does NOT perform the device-number claim handshake — safe to call before connecting.
pub async fn discover_cdjs(timeout_ms: u64) -> Vec<DiscoveredDevice> {
    let Ok(sock) = bind_announce_socket() else {
        return vec![];
    };
    let mut buf = [0u8; 1500];
    let mut discovered: HashMap<u8, DiscoveredDevice> = HashMap::new();
    let deadline = sleep(Duration::from_millis(timeout_ms));
    tokio::pin!(deadline);

    loop {
        tokio::select! {
            _ = &mut deadline => break,
            result = sock.recv_from(&mut buf) => {
                if let Ok((len, addr)) = result {
                    let data = &buf[..len];
                    if let Some(pkt) = parse_keepalive(data) {
                        let src_ip = match addr {
                            std::net::SocketAddr::V4(a) => a.ip().to_string(),
                            std::net::SocketAddr::V6(_) => pkt.ip.to_string(),
                        };
                        // Accept CDJ players (0x01, 0x03); skip mixers (0x02)
                        if pkt.device_type != DEVICE_TYPE_MIXER && !discovered.contains_key(&pkt.player) {
                            discovered.insert(pkt.player, DiscoveredDevice {
                                player: pkt.player,
                                name: pkt.name,
                                ip: src_ip,
                            });
                        }
                    }
                }
            }
        }
    }

    let mut result: Vec<DiscoveredDevice> = discovered.into_values().collect();
    result.sort_by_key(|d| d.player);
    result
}

// ── Public event + state types ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum ProDJLinkEvent {
    Discovered {
        ip: String,
        name: String,
        player: u8,
    },
    Connected,
    StateChanged(ProDJSnapshot),
    Disconnected,
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ProDJSnapshot {
    pub decks: Vec<ProDJDeckState>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProDJDeckState {
    pub player: u8,
    pub cdj_ip: String,
    pub slot: u8,
    pub rekordbox_id: u32,
    pub title: String,
    pub artist: String,
    pub duration_secs: u32,
    pub playing: bool,
    pub on_air: bool,
    pub master: bool,
    pub bpm: f64,
    pub effective_bpm: f64,
    pub beat_number: u32,
    pub position_ms: u32,
    pub track_length_s: u32,
}

impl Default for ProDJDeckState {
    fn default() -> Self {
        Self {
            player: 0,
            cdj_ip: String::new(),
            slot: 0,
            rekordbox_id: 0,
            title: String::new(),
            artist: String::new(),
            duration_secs: 0,
            playing: false,
            on_air: false,
            master: false,
            bpm: 0.0,
            effective_bpm: 0.0,
            beat_number: 0,
            position_ms: 0,
            track_length_s: 0,
        }
    }
}

/// Handle to a running Pro DJ Link session.
pub struct ProDJLinkClient {
    stop_tx: mpsc::Sender<()>,
}

impl ProDJLinkClient {
    /// Start the Pro DJ Link client.
    ///
    /// `device_num` is the virtual player number Luma claims on the network (default: 7).
    /// CDJs use 1-4, so any value ≥ 5 is safe.
    ///
    /// Spawns a background task that:
    /// 1. Binds UDP sockets on ports 50000, 50001, 50002
    /// 2. Sends the CDJ device-number claim handshake (~3.6 s)
    /// 3. Emits [`ProDJLinkEvent::Connected`]
    /// 4. Enters the main event loop (keepalive, status, position, metadata)
    pub async fn start(
        device_num: u8,
        callback: impl Fn(ProDJLinkEvent) + Send + Sync + 'static,
    ) -> Result<Self, String> {
        let our_ip = find_local_ipv4().ok_or("No non-loopback IPv4 interface found")?;

        let announce_sock = bind_announce_socket().map_err(|e| e.to_string())?;
        let status_sock = bind_udp_port(STATUS_PORT)
            .await
            .map_err(|e| format!("bind port {STATUS_PORT}: {e}"))?;
        let position_sock = bind_udp_port(POSITION_PORT)
            .await
            .map_err(|e| format!("bind port {POSITION_PORT}: {e}"))?;

        let (stop_tx, stop_rx) = mpsc::channel::<()>(1);
        let cb = Arc::new(callback);

        tokio::spawn(async move {
            run_client(
                announce_sock,
                status_sock,
                position_sock,
                stop_rx,
                cb,
                our_ip,
                device_num,
            )
            .await;
        });

        Ok(Self { stop_tx })
    }

    /// Stop the client and disconnect from the network.
    pub async fn stop(self) {
        let _ = self.stop_tx.send(()).await;
    }
}

// ── Internal deck state ───────────────────────────────────────────────────────

struct DeckEntry {
    outer: ProDJDeckState,
    /// The rekordbox_id currently being fetched (prevents duplicate fetches).
    pending_rekordbox_id: Option<u32>,
}

// ── Main async task ───────────────────────────────────────────────────────────

async fn run_client(
    announce_sock: UdpSocket,
    status_sock: UdpSocket,
    position_sock: UdpSocket,
    mut stop_rx: mpsc::Receiver<()>,
    callback: Arc<dyn Fn(ProDJLinkEvent) + Send + Sync>,
    our_ip: Ipv4Addr,
    device_num: u8,
) {
    // ── Claim handshake ───────────────────────────────────────────────────────
    let broadcast: SocketAddr = "255.255.255.255:50000".parse().unwrap();

    let hello = build_hello();
    let stage1: Vec<_> = (1..=3).map(build_stage1).collect();
    let stage2: Vec<_> = (1..=3)
        .map(|i| build_stage2(our_ip, i, device_num))
        .collect();
    let stage3: Vec<_> = (1..=3).map(|i| build_stage3(i, device_num)).collect();
    let delay = Duration::from_millis(CLAIM_STEP_MS);

    for _ in 0..3u8 {
        let _ = announce_sock.send_to(&hello, broadcast).await;
        sleep(delay).await;
    }
    for pkt in &stage1 {
        let _ = announce_sock.send_to(pkt, broadcast).await;
        sleep(delay).await;
    }
    for pkt in &stage2 {
        let _ = announce_sock.send_to(pkt, broadcast).await;
        sleep(delay).await;
    }
    for pkt in &stage3 {
        let _ = announce_sock.send_to(pkt, broadcast).await;
        sleep(delay).await;
    }

    callback(ProDJLinkEvent::Connected);

    // ── Event loop ────────────────────────────────────────────────────────────
    let (meta_tx, mut meta_rx) = mpsc::channel::<MetadataResult>(32);
    let mut keepalive_tick = interval(Duration::from_millis(KEEPALIVE_INTERVAL_MS));
    let mut decks: HashMap<u8, DeckEntry> = HashMap::new();
    let mut announce_buf = [0u8; 1500];
    let mut status_buf = [0u8; 1500];
    let mut position_buf = [0u8; 1500];

    loop {
        tokio::select! {
            // ── Stop signal ───────────────────────────────────────────────────
            _ = stop_rx.recv() => {
                callback(ProDJLinkEvent::Disconnected);
                break;
            }

            // ── Keepalive broadcast ───────────────────────────────────────────
            _ = keepalive_tick.tick() => {
                let peer_count = decks.len() as u8;
                let pkt = build_keepalive(our_ip, peer_count, device_num);
                let _ = announce_sock.send_to(&pkt, broadcast).await;
            }

            // ── Port 50000: CDJ keepalive announcements ───────────────────────
            result = announce_sock.recv_from(&mut announce_buf) => {
                if let Ok((len, addr)) = result {
                    let data = &announce_buf[..len];
                    if let Some(pkt) = parse_keepalive(data) {
                        if pkt.ip == our_ip || pkt.player == device_num {
                            continue;
                        }
                        // Skip mixers (0x02); accept CDJ-NXS (0x01) and CDJ-3000 (0x03)
                        if pkt.device_type == DEVICE_TYPE_MIXER {
                            continue;
                        }
                        if !decks.contains_key(&pkt.player) {
                            let src_ip = match addr {
                                std::net::SocketAddr::V4(a) => a.ip().to_string(),
                                std::net::SocketAddr::V6(_) => pkt.ip.to_string(),
                            };
                            callback(ProDJLinkEvent::Discovered {
                                ip: src_ip.clone(),
                                name: pkt.name.clone(),
                                player: pkt.player,
                            });
                            decks.insert(pkt.player, DeckEntry {
                                outer: ProDJDeckState {
                                    player: pkt.player,
                                    cdj_ip: src_ip,
                                    ..Default::default()
                                },
                                pending_rekordbox_id: None,
                            });
                            emit_snapshot(&decks, &callback);
                        }
                    }
                }
            }

            // ── Port 50002: CDJ status ────────────────────────────────────────
            result = status_sock.recv_from(&mut status_buf) => {
                if let Ok((len, _)) = result {
                    let data = &status_buf[..len];
                    if let Some(pkt) = parse_status(data, device_num) {
                        apply_status(&mut decks, pkt, &meta_tx);
                        emit_snapshot(&decks, &callback);
                    }
                }
            }

            // ── Port 50001: precise position ──────────────────────────────────
            result = position_sock.recv_from(&mut position_buf) => {
                if let Ok((len, _)) = result {
                    let data = &position_buf[..len];
                    if let Some(pkt) = parse_position(data, device_num) {
                        if let Some(deck) = decks.get_mut(&pkt.player) {
                            deck.outer.position_ms = pkt.position_ms;
                            deck.outer.track_length_s = pkt.track_length_s;
                            deck.outer.effective_bpm = pkt.effective_bpm();
                        }
                        emit_snapshot(&decks, &callback);
                    }
                }
            }

            // ── Metadata fetch results ────────────────────────────────────────
            Some(result) = meta_rx.recv() => {
                apply_metadata(&mut decks, result);
                emit_snapshot(&decks, &callback);
            }
        }
    }
}

// ── State update helpers ──────────────────────────────────────────────────────

fn apply_status(
    decks: &mut HashMap<u8, DeckEntry>,
    pkt: packets::StatusPacket,
    meta_tx: &mpsc::Sender<MetadataResult>,
) {
    let deck = match decks.get_mut(&pkt.player) {
        Some(d) => d,
        None => return,
    };

    deck.outer.slot = pkt.slot;
    deck.outer.playing = (pkt.flags & FLAG_PLAYING) != 0;
    deck.outer.on_air = (pkt.flags & FLAG_ON_AIR) != 0;
    deck.outer.master = (pkt.flags & FLAG_MASTER) != 0;
    deck.outer.bpm = pkt.bpm();
    deck.outer.beat_number = pkt.beat_number;

    let new_id = pkt.rekordbox_id;
    if new_id == 0 {
        // Track ejected
        if deck.outer.rekordbox_id != 0 || deck.pending_rekordbox_id.is_some() {
            deck.outer.rekordbox_id = 0;
            deck.outer.title = String::new();
            deck.outer.artist = String::new();
            deck.outer.duration_secs = 0;
            deck.outer.position_ms = 0;
            deck.outer.track_length_s = 0;
            deck.pending_rekordbox_id = None;
        }
    } else if new_id != deck.outer.rekordbox_id && Some(new_id) != deck.pending_rekordbox_id {
        // New track loaded — fetch metadata from the source player's dbserver
        // (the USB/SD may be in a different CDJ than the one playing the track)
        deck.pending_rekordbox_id = Some(new_id);

        let source_ip = if pkt.track_source_player != pkt.player {
            // Track loaded from another CDJ's media — find that CDJ's IP
            let fallback = deck.outer.cdj_ip.clone();
            decks
                .get(&pkt.track_source_player)
                .map(|d| d.outer.cdj_ip.clone())
                .unwrap_or(fallback)
        } else {
            deck.outer.cdj_ip.clone()
        };

        let req = MetadataRequest {
            cdj_ip: source_ip.parse().unwrap_or(Ipv4Addr::UNSPECIFIED),
            player: pkt.track_source_player,
            slot: pkt.slot,
            rekordbox_id: new_id,
            deck_key: pkt.player,
        };
        let tx = meta_tx.clone();
        tokio::spawn(async move {
            let result = fetch_metadata(req).await;
            let _ = tx.send(result).await;
        });
    }
}

fn apply_metadata(decks: &mut HashMap<u8, DeckEntry>, result: MetadataResult) {
    let deck = match decks.get_mut(&result.deck_key) {
        Some(d) => d,
        None => return,
    };
    // Guard against stale results (track may have changed again while fetch was in flight)
    if deck.pending_rekordbox_id != Some(result.rekordbox_id) {
        return;
    }
    deck.outer.rekordbox_id = result.rekordbox_id;
    deck.outer.title = result.title;
    deck.outer.artist = result.artist;
    deck.outer.duration_secs = result.duration_secs;
    deck.pending_rekordbox_id = None;
}

fn emit_snapshot(
    decks: &HashMap<u8, DeckEntry>,
    callback: &Arc<dyn Fn(ProDJLinkEvent) + Send + Sync>,
) {
    let mut deck_list: Vec<ProDJDeckState> = decks.values().map(|d| d.outer.clone()).collect();
    deck_list.sort_by_key(|d| d.player);
    callback(ProDJLinkEvent::StateChanged(ProDJSnapshot {
        decks: deck_list,
    }));
}

// ── Network helpers ───────────────────────────────────────────────────────────

/// Bind the announce socket on port 50000 with SO_REUSEADDR + SO_REUSEPORT + SO_BROADCAST.
fn bind_announce_socket() -> std::io::Result<UdpSocket> {
    let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    sock.set_reuse_address(true)?;
    #[cfg(unix)]
    sock.set_reuse_port(true)?;
    sock.set_broadcast(true)?;
    sock.set_nonblocking(true)?;
    sock.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, ANNOUNCE_PORT).into())?;
    let std_sock: std::net::UdpSocket = sock.into();
    UdpSocket::from_std(std_sock)
}

/// Kill any process holding `port`, then bind a UDP socket on it.
async fn bind_udp_port(port: u16) -> std::io::Result<UdpSocket> {
    kill_port_holder(port);
    UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port)).await
}

/// Kill whatever process is listening on `port` using lsof.
fn kill_port_holder(port: u16) {
    let output = std::process::Command::new("lsof")
        .args(["-ti", &format!(":{port}")])
        .output();
    if let Ok(out) = output {
        let pids = String::from_utf8_lossy(&out.stdout);
        for pid_str in pids.split_whitespace() {
            if let Ok(pid) = pid_str.parse::<u32>() {
                eprintln!("[prodjlink] killing pid {pid} holding port {port}");
                let _ = std::process::Command::new("kill")
                    .args(["-9", &pid.to_string()])
                    .status();
            }
        }
    }
}

/// Find the first non-loopback, non-link-local IPv4 address on this machine.
fn find_local_ipv4() -> Option<Ipv4Addr> {
    let ifaces = if_addrs::get_if_addrs().ok()?;
    // Prefer non-link-local (169.254.x.x)
    for iface in &ifaces {
        if iface.is_loopback() {
            continue;
        }
        if let if_addrs::IfAddr::V4(v4) = &iface.addr {
            let octets = v4.ip.octets();
            if octets[0] != 169 || octets[1] != 254 {
                return Some(v4.ip);
            }
        }
    }
    // Fall back to link-local
    for iface in &ifaces {
        if iface.is_loopback() {
            continue;
        }
        if let if_addrs::IfAddr::V4(v4) = &iface.addr {
            return Some(v4.ip);
        }
    }
    None
}
