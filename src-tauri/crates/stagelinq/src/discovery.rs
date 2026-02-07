use std::collections::HashSet;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::{interval, Duration};

use crate::protocol::{build_discovery_message, parse_discovery_message};
use crate::types::*;

/// Per-interface broadcast info: (local_ip, broadcast_addr).
/// On multi-homed systems (WiFi + Ethernet), we need to send broadcasts from a socket
/// bound to each interface's local IP so the OS routes them out the correct NIC.
fn find_interface_broadcasts() -> Vec<(Ipv4Addr, SocketAddr)> {
    let mut result = Vec::new();
    let ifaces = match if_addrs::get_if_addrs() {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    for iface in ifaces {
        if iface.is_loopback() {
            continue;
        }
        if let if_addrs::IfAddr::V4(v4) = &iface.addr {
            let bcast_ip = if let Some(broadcast) = v4.broadcast {
                broadcast
            } else {
                let ip_u32 = u32::from_be_bytes(v4.ip.octets());
                let mask_u32 = u32::from_be_bytes(v4.netmask.octets());
                Ipv4Addr::from((ip_u32 | !mask_u32).to_be_bytes())
            };
            result.push((
                v4.ip,
                SocketAddr::V4(SocketAddrV4::new(bcast_ip, DISCOVERY_PORT)),
            ));
        }
    }
    result
}

/// A device discovered on the network.
#[derive(Debug, Clone)]
pub struct DiscoveredDevice {
    pub address: Ipv4Addr,
    pub port: u16,
    pub token: [u8; 16],
    pub source: String,
    pub software_name: String,
    pub software_version: String,
}

/// Run the discovery loop: broadcast our presence and listen for device announcements.
///
/// Discovered devices are sent on the returned channel.
/// Drop the returned `DiscoveryHandle` to stop discovery.
pub async fn run_discovery(
    our_token: [u8; 16],
) -> std::io::Result<(DiscoveryHandle, mpsc::Receiver<DiscoveredDevice>)> {
    // Listener socket on the well-known port — receives device announcements from all interfaces.
    let listener =
        UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, DISCOVERY_PORT)).await?;
    listener.set_broadcast(true)?;

    // Per-interface announce sockets — each bound to a local IP so broadcasts go out the
    // correct NIC. On macOS, a socket bound to 0.0.0.0 may route broadcasts through the
    // wrong interface on multi-homed systems (e.g. WiFi + Ethernet).
    let iface_broadcasts = find_interface_broadcasts();
    let mut announce_sockets: Vec<(UdpSocket, SocketAddr)> = Vec::new();
    for (local_ip, bcast_addr) in &iface_broadcasts {
        match UdpSocket::bind(SocketAddrV4::new(*local_ip, 0)).await {
            Ok(s) => {
                let _ = s.set_broadcast(true);
                eprintln!(
                    "[stagelinq::discovery] announce socket: {} -> {}",
                    local_ip, bcast_addr
                );
                announce_sockets.push((s, *bcast_addr));
            }
            Err(e) => {
                eprintln!(
                    "[stagelinq::discovery] failed to bind announce socket to {}: {}",
                    local_ip, e
                );
            }
        }
    }
    // Fallback: if no per-interface sockets, use the listener socket with generic broadcast.
    let use_listener_for_announce = announce_sockets.is_empty();
    if use_listener_for_announce {
        eprintln!("[stagelinq::discovery] no per-interface sockets, falling back to listener for announces");
    }

    let (tx, rx) = mpsc::channel::<DiscoveredDevice>(32);
    let (stop_tx, mut stop_rx) = mpsc::channel::<()>(1);

    let announce_msg = build_discovery_message(
        &our_token,
        SOFTWARE_SOURCE,
        ACTION_LOGIN,
        SOFTWARE_NAME,
        SOFTWARE_VERSION,
        0, // we don't accept incoming connections
    );

    let exit_msg = build_discovery_message(
        &our_token,
        SOFTWARE_SOURCE,
        ACTION_LOGOUT,
        SOFTWARE_NAME,
        SOFTWARE_VERSION,
        0,
    );

    let fallback_target = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::BROADCAST, DISCOVERY_PORT));

    tokio::spawn(async move {
        let mut ticker = interval(Duration::from_millis(ANNOUNCEMENT_INTERVAL_MS));
        let mut buf = [0u8; 4096];
        let mut seen: HashSet<(Ipv4Addr, u16)> = HashSet::new();

        loop {
            tokio::select! {
                _ = stop_rx.recv() => {
                    // Send logout on all interfaces
                    if use_listener_for_announce {
                        let _ = listener.send_to(&exit_msg, fallback_target).await;
                    } else {
                        for (sock, bcast) in &announce_sockets {
                            let _ = sock.send_to(&exit_msg, bcast).await;
                        }
                    }
                    break;
                }
                _ = ticker.tick() => {
                    if use_listener_for_announce {
                        let _ = listener.send_to(&announce_msg, fallback_target).await;
                    } else {
                        for (sock, bcast) in &announce_sockets {
                            let _ = sock.send_to(&announce_msg, bcast).await;
                        }
                    }
                }
                result = listener.recv_from(&mut buf) => {
                    match result {
                        Ok((len, addr)) => {
                            let data = &buf[..len];
                            if let Ok(msg) = parse_discovery_message(data) {
                                // Ignore our own announcements (match by source name)
                                if msg.source == SOFTWARE_SOURCE {
                                    continue;
                                }
                                // Ignore logout messages
                                if msg.action == ACTION_LOGOUT {
                                    continue;
                                }
                                // Ignore devices with no listening port
                                if msg.port == 0 {
                                    continue;
                                }
                                // Ignore known non-player devices
                                if msg.software_name == "OfflineAnalyzer"
                                    || msg.software_name.starts_with("SoundSwitch")
                                    || msg.software_name.starts_with("Resolume")
                                    || msg.software_name == "JM08"
                                    || msg.software_name == "SSS0"
                                {
                                    continue;
                                }
                                let ip = match addr {
                                    SocketAddr::V4(v4) => *v4.ip(),
                                    SocketAddr::V6(_) => continue,
                                };
                                let key = (ip, msg.port);
                                if !seen.insert(key) {
                                    continue; // already discovered this device
                                }
                                eprintln!("[stagelinq::discovery] device found: {} {} v{} at {}:{}", msg.software_name, msg.source, msg.software_version, ip, msg.port);
                                let device = DiscoveredDevice {
                                    address: ip,
                                    port: msg.port,
                                    token: msg.token,
                                    source: msg.source,
                                    software_name: msg.software_name,
                                    software_version: msg.software_version,
                                };
                                if tx.send(device).await.is_err() {
                                    break; // receiver dropped
                                }
                            }
                        }
                        Err(_) => continue,
                    }
                }
            }
        }
    });

    Ok((DiscoveryHandle { stop: stop_tx }, rx))
}

/// Handle to stop the discovery loop.
pub struct DiscoveryHandle {
    stop: mpsc::Sender<()>,
}

impl DiscoveryHandle {
    pub async fn stop(self) {
        let _ = self.stop.send(()).await;
    }
}
