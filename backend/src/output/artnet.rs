//! Art-Net: DMX over UDP to nodes on the network.
//!
//! One thread owns the socket. While output is on it polls for nodes every
//! three seconds, collects their replies, and sends each new frame to every
//! routed node port. While output is off it holds no socket at all.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::{OutputDriver, Shared};
use crate::models::patch::{DeviceKind, DmxRoute, OutputDevice, OutputPort};

const ARTNET_PORT: u16 = 6454;
const HEADER: &[u8] = b"Art-Net\0";
const POLL_INTERVAL: Duration = Duration::from_secs(3);
/// A node that has missed this many polls is gone.
const NODE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long one receive may wait, which bounds how late a new frame is sent.
const RECV_WAIT: Duration = Duration::from_millis(5);
/// The id of the pseudo-device that sends to the whole subnet.
const BROADCAST_ID: &str = "broadcast";

/// A node that answered a poll, as of its latest reply.
struct Node {
    device: OutputDevice,
    ip: IpAddr,
    last_seen: Instant,
}

pub(super) struct ArtNetDriver {
    nodes: Arc<Mutex<HashMap<String, Node>>>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl ArtNetDriver {
    pub fn start(shared: Arc<Shared>) -> Self {
        let nodes = Arc::new(Mutex::new(HashMap::new()));
        let thread = std::thread::Builder::new()
            .name("luma-artnet".into())
            .spawn({
                let shared = shared.clone();
                let nodes = nodes.clone();
                move || run(&shared, &nodes)
            })
            .map_err(|e| log::error!("[ArtNet] could not start its thread: {e}"))
            .ok();
        Self {
            nodes,
            shared,
            thread,
        }
    }
}

impl OutputDriver for ArtNetDriver {
    fn devices(&self) -> Vec<OutputDevice> {
        if !self.shared.enabled.load(Ordering::Relaxed) {
            return Vec::new();
        }
        let nodes = self.nodes.lock().unwrap_or_else(|e| e.into_inner());
        let mut devices: Vec<OutputDevice> = nodes.values().map(|n| n.device.clone()).collect();
        devices.sort_by(|a, b| a.name.cmp(&b.name).then(a.address.cmp(&b.address)));
        devices.push(OutputDevice {
            kind: DeviceKind::ArtNet,
            id: BROADCAST_ID.into(),
            name: "Art-Net broadcast".into(),
            address: "255.255.255.255".into(),
            ports: Vec::new(),
            any_port: true,
            problem: None,
        });
        devices
    }
}

impl Drop for ArtNetDriver {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The socket and what it was bound for.
struct Bound {
    socket: UdpSocket,
    interface: String,
    broadcasts: Vec<SocketAddr>,
}

fn run(shared: &Shared, nodes: &Mutex<HashMap<String, Node>>) {
    let mut bound: Option<Bound> = None;
    let mut last_bind_attempt: Option<Instant> = None;
    let mut last_poll: Option<Instant> = None;
    let mut sent = 0u64;
    let mut sequence = 0u8;
    let mut buf = [0u8; 1024];

    while shared.running() {
        let interface = shared
            .artnet_interface
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if !shared.enabled.load(Ordering::Relaxed) {
            if bound.take().is_some() {
                log::info!("[ArtNet] output off; socket closed");
            }
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        if bound.as_ref().is_some_and(|b| b.interface != interface) {
            bound = None;
        }
        let Some(socket) = (match &bound {
            Some(b) => Some(b),
            None => {
                // A port another program holds is retried, not hammered.
                if last_bind_attempt.is_some_and(|t| t.elapsed() < Duration::from_secs(2)) {
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
                last_bind_attempt = Some(Instant::now());
                bound = bind(&interface);
                last_poll = None;
                bound.as_ref()
            }
        }) else {
            continue;
        };

        if last_poll.is_none_or(|t| t.elapsed() >= POLL_INTERVAL) {
            let poll = build_artpoll_packet();
            for target in &socket.broadcasts {
                let _ = socket.socket.send_to(&poll, target);
            }
            last_poll = Some(Instant::now());
            nodes
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .retain(|_, node| node.last_seen.elapsed() < NODE_TIMEOUT);
        }

        if let Ok((size, from)) = socket.socket.recv_from(&mut buf) {
            if let Some(reply) = parse_poll_reply(&buf[..size], from.ip()) {
                let mut nodes = nodes.lock().unwrap_or_else(|e| e.into_inner());
                merge_reply(&mut nodes, reply);
            }
        }

        send_newer(shared, nodes, socket, &mut sent, &mut sequence);
    }
    // The host publishes a dark frame as it shuts down; send it before the
    // socket goes, so the rig does not hold its last look.
    if let Some(socket) = &bound {
        send_newer(shared, nodes, socket, &mut sent, &mut sequence);
    }
}

/// Send the newest frame to every routed node port, if there is one newer
/// than `sent`.
fn send_newer(
    shared: &Shared,
    nodes: &Mutex<HashMap<String, Node>>,
    socket: &Bound,
    sent: &mut u64,
    sequence: &mut u8,
) {
    let Some((seq, frame)) = shared.frames.newer_than(*sent, Duration::ZERO) else {
        return;
    };
    *sent = seq;
    let routes = shared.routes();
    let nodes = nodes.lock().unwrap_or_else(|e| e.into_inner());
    for route in routes
        .iter()
        .filter(|r| r.device_kind == DeviceKind::ArtNet)
    {
        let Some(data) = frame.universes.get(&route.universe) else {
            continue;
        };
        let packet = build_artdmx_packet(*sequence, route.port as u16, data);
        for target in destinations(route, &nodes, &socket.broadcasts) {
            let _ = socket.socket.send_to(&packet, target);
        }
    }
    *sequence = sequence.wrapping_add(1);
}

/// Where one route's packets go: the node's address as of its last reply,
/// else the address stored when it was bound, else nowhere.
fn destinations(
    route: &DmxRoute,
    nodes: &HashMap<String, Node>,
    broadcasts: &[SocketAddr],
) -> Vec<SocketAddr> {
    if route.device_id == BROADCAST_ID {
        return broadcasts.to_vec();
    }
    // An id is a MAC, or an IP for a node that announces none and for
    // bindings made before ids were MACs.
    let live = nodes
        .get(&route.device_id)
        .or_else(|| nodes.values().find(|n| n.ip.to_string() == route.device_id));
    if let Some(node) = live {
        return vec![SocketAddr::new(node.ip, ARTNET_PORT)];
    }
    route
        .address
        .as_deref()
        .and_then(|a| {
            a.parse::<SocketAddr>().ok().or_else(|| {
                a.parse::<IpAddr>()
                    .ok()
                    .map(|ip| SocketAddr::new(ip, ARTNET_PORT))
            })
        })
        .into_iter()
        .collect()
}

fn bind(interface: &str) -> Option<Bound> {
    let ip = if interface.is_empty() {
        "0.0.0.0"
    } else {
        interface
    };
    let socket = match UdpSocket::bind((ip, ARTNET_PORT)) {
        Ok(socket) => socket,
        Err(e) if ip != "0.0.0.0" => {
            log::warn!("[ArtNet] could not bind {ip}:{ARTNET_PORT} ({e}); trying every interface");
            UdpSocket::bind(("0.0.0.0", ARTNET_PORT))
                .map_err(|e| log::error!("[ArtNet] could not bind port {ARTNET_PORT}: {e}"))
                .ok()?
        }
        Err(e) => {
            log::error!("[ArtNet] could not bind port {ARTNET_PORT}: {e}");
            return None;
        }
    };
    let _ = socket.set_broadcast(true);
    let _ = socket.set_read_timeout(Some(RECV_WAIT));
    log::info!(
        "[ArtNet] bound {}",
        socket
            .local_addr()
            .map_or_else(|_| ip.to_string(), |a| a.to_string())
    );
    Some(Bound {
        socket,
        interface: interface.to_string(),
        broadcasts: broadcast_targets(),
    })
}

/// The limited broadcast, plus the directed one for this machine's subnet
/// (assumed /24): some nodes and switches drop 255.255.255.255.
fn broadcast_targets() -> Vec<SocketAddr> {
    let mut targets = vec![SocketAddr::from(([255, 255, 255, 255], ARTNET_PORT))];
    let local = UdpSocket::bind("0.0.0.0:0").and_then(|probe| {
        probe
            .connect("8.8.8.8:80")
            .and_then(|()| probe.local_addr())
    });
    if let Ok(SocketAddr::V4(local)) = local {
        let [a, b, c, _] = local.ip().octets();
        targets.push(SocketAddr::from(([a, b, c, 255], ARTNET_PORT)));
    }
    targets
}

/// One ArtPollReply, reduced to what a device needs.
struct PollReply {
    ip: IpAddr,
    mac: Option<[u8; 6]>,
    short_name: String,
    long_name: String,
    ports: Vec<u16>,
}

/// Parse an ArtPollReply (Art-Net 4). Offsets are the spec's: Net and SubNet
/// at 18 and 19, the names at 26 and 44, NumPorts at 173, PortTypes at
/// 174..178, SwOut at 190..194, and the MAC at 201..207.
fn parse_poll_reply(buf: &[u8], from: IpAddr) -> Option<PollReply> {
    if buf.len() < 108 || &buf[0..8] != HEADER {
        return None;
    }
    let opcode = u16::from_le_bytes([buf[8], buf[9]]);
    if opcode != 0x2100 {
        return None;
    }
    let text = |range: std::ops::Range<usize>| {
        String::from_utf8_lossy(&buf[range])
            .trim_matches(char::from(0))
            .trim()
            .to_string()
    };
    let base = (u16::from(buf[18] & 0x7F) << 8) | (u16::from(buf[19] & 0x0F) << 4);
    let mut ports = Vec::new();
    if buf.len() >= 194 {
        let count = usize::from(buf[173]).min(4);
        for i in 0..count {
            // Bit 7 of PortTypes: this port can output DMX from Art-Net.
            if buf[174 + i] & 0x80 != 0 {
                ports.push(base | u16::from(buf[190 + i] & 0x0F));
            }
        }
    }
    if ports.is_empty() {
        ports.push(base);
    }
    let mac = (buf.len() >= 207)
        .then(|| <[u8; 6]>::try_from(&buf[201..207]).ok())
        .flatten()
        .filter(|mac| mac.iter().any(|b| *b != 0));
    Some(PollReply {
        ip: from,
        mac,
        short_name: text(26..44),
        long_name: text(44..108),
        ports,
    })
}

/// Fold one reply into the node table. A node with more than four ports
/// answers once per group of four, so ports accumulate under one id.
fn merge_reply(nodes: &mut HashMap<String, Node>, reply: PollReply) {
    let id = reply.mac.map_or_else(
        || reply.ip.to_string(),
        |mac| {
            mac.iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(":")
        },
    );
    let name = if reply.short_name.is_empty() {
        reply.long_name.clone()
    } else {
        reply.short_name.clone()
    };
    let node = nodes.entry(id.clone()).or_insert_with(|| Node {
        device: OutputDevice {
            kind: DeviceKind::ArtNet,
            id,
            name: String::new(),
            address: String::new(),
            ports: Vec::new(),
            any_port: false,
            problem: None,
        },
        ip: reply.ip,
        last_seen: Instant::now(),
    });
    node.ip = reply.ip;
    node.last_seen = Instant::now();
    node.device.name = name;
    node.device.address = reply.ip.to_string();
    for port in reply.ports {
        if !node.device.ports.iter().any(|p| p.port == i64::from(port)) {
            node.device.ports.push(OutputPort {
                port: i64::from(port),
                label: port_label(port),
            });
        }
    }
    node.device.ports.sort_by_key(|p| p.port);
}

/// `net.subnet.universe`, the way node configuration pages print it.
fn port_label(port: u16) -> String {
    format!("{}.{}.{}", port >> 8, (port >> 4) & 0xF, port & 0xF)
}

fn build_artdmx_packet(sequence: u8, port_address: u16, data: &[u8; 512]) -> Vec<u8> {
    let mut packet = Vec::with_capacity(18 + 512);
    packet.extend_from_slice(HEADER);
    packet.extend_from_slice(&0x5000u16.to_le_bytes()); // OpDmx, low byte first
    packet.extend_from_slice(&14u16.to_be_bytes()); // ProtVer
    packet.push(sequence);
    packet.push(0x00); // Physical
    packet.push((port_address & 0xFF) as u8); // SubUni
    packet.push(((port_address >> 8) & 0x7F) as u8); // Net
    packet.extend_from_slice(&512u16.to_be_bytes()); // Length
    packet.extend_from_slice(data);
    packet
}

fn build_artpoll_packet() -> Vec<u8> {
    let mut packet = Vec::with_capacity(14);
    packet.extend_from_slice(HEADER);
    packet.extend_from_slice(&0x2000u16.to_le_bytes()); // OpPoll
    packet.extend_from_slice(&14u16.to_be_bytes()); // ProtVer
    packet.push(0x00); // Flags
    packet.push(0x00); // DiagPriority
    packet
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(mac: [u8; 6], port_types: [u8; 4], sw_out: [u8; 4]) -> Vec<u8> {
        let mut buf = vec![0u8; 239];
        buf[0..8].copy_from_slice(HEADER);
        buf[8..10].copy_from_slice(&0x2100u16.to_le_bytes());
        buf[18] = 1; // Net
        buf[19] = 2; // SubNet
        buf[26..32].copy_from_slice(b"Node A");
        buf[173] = 4;
        buf[174..178].copy_from_slice(&port_types);
        buf[190..194].copy_from_slice(&sw_out);
        buf[201..207].copy_from_slice(&mac);
        buf
    }

    #[test]
    fn a_reply_names_its_output_ports_and_mac() {
        let ip: IpAddr = "10.0.0.5".parse().unwrap();
        let buf = reply([0, 1, 2, 3, 4, 5], [0x80, 0x80, 0x40, 0x00], [3, 4, 5, 6]);
        let parsed = parse_poll_reply(&buf, ip).unwrap();
        assert_eq!(parsed.mac, Some([0, 1, 2, 3, 4, 5]));
        assert_eq!(parsed.short_name, "Node A");
        // Only the two output-capable ports, on Net 1 / SubNet 2.
        assert_eq!(parsed.ports, vec![0x123, 0x124]);
    }

    #[test]
    fn replies_from_one_node_accumulate_under_its_mac() {
        let ip: IpAddr = "10.0.0.5".parse().unwrap();
        let mut nodes = HashMap::new();
        let mac = [0xaa, 0, 0, 0, 0, 1];
        merge_reply(
            &mut nodes,
            parse_poll_reply(&reply(mac, [0x80, 0, 0, 0], [0, 0, 0, 0]), ip).unwrap(),
        );
        merge_reply(
            &mut nodes,
            parse_poll_reply(&reply(mac, [0x80, 0, 0, 0], [1, 0, 0, 0]), ip).unwrap(),
        );
        assert_eq!(nodes.len(), 1);
        let node = &nodes["aa:00:00:00:00:01"].device;
        assert_eq!(
            node.ports.iter().map(|p| p.port).collect::<Vec<_>>(),
            vec![0x120, 0x121]
        );
        assert_eq!(node.ports[1].label, "1.2.1");
    }

    #[test]
    fn a_route_finds_its_node_by_mac_or_old_ip_else_its_stored_address() {
        let ip: IpAddr = "10.0.0.9".parse().unwrap();
        let mut nodes = HashMap::new();
        merge_reply(
            &mut nodes,
            parse_poll_reply(&reply([0xaa, 0, 0, 0, 0, 1], [0x80, 0, 0, 0], [0; 4]), ip).unwrap(),
        );
        let route = |id: &str, address: Option<&str>| DmxRoute {
            universe: 1,
            device_kind: DeviceKind::ArtNet,
            device_id: id.into(),
            port: 0,
            device_name: None,
            address: address.map(Into::into),
        };
        let at = |s: &str| vec![s.parse::<SocketAddr>().unwrap()];
        assert_eq!(
            destinations(
                &route("aa:00:00:00:00:01", Some("10.0.0.1:6454")),
                &nodes,
                &[]
            ),
            at("10.0.0.9:6454")
        );
        assert_eq!(
            destinations(&route("10.0.0.9", None), &nodes, &[]),
            at("10.0.0.9:6454")
        );
        assert_eq!(
            destinations(&route("10.0.0.7", Some("10.0.0.7:6454")), &nodes, &[]),
            at("10.0.0.7:6454")
        );
        assert!(destinations(&route("gone", None), &nodes, &[]).is_empty());
    }

    #[test]
    fn artdmx_carries_the_port_address_split_into_subuni_and_net() {
        let packet = build_artdmx_packet(7, 0x123, &[9; 512]);
        assert_eq!(&packet[8..10], &[0x00, 0x50]);
        assert_eq!(packet[12], 7);
        assert_eq!(packet[14], 0x23);
        assert_eq!(packet[15], 0x01);
        assert_eq!(&packet[16..18], &[0x02, 0x00]);
        assert_eq!(packet.len(), 18 + 512);
    }
}
