use std::collections::HashSet;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::{interval, Duration};

use crate::protocol::{build_discovery_message, parse_discovery_message};
use crate::types::*;

/// Compute subnet-specific broadcast addresses for all non-loopback IPv4 interfaces.
/// The node lib does this too — sending to 255.255.255.255 may go out on the wrong
/// interface on multi-homed systems, so broadcasts would never reach the DJ hardware.
fn find_broadcast_addresses() -> Vec<SocketAddr> {
    let output = match std::process::Command::new("ip")
        .args(["-4", "-o", "addr", "show"])
        .output()
    {
        Ok(o) => o,
        Err(_) => return vec![],
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let mut addrs = Vec::new();

    for line in text.lines() {
        if line.contains(" lo ") {
            continue;
        }
        if let Some(inet_pos) = line.find("inet ") {
            let rest = &line[inet_pos + 5..];
            if let Some(slash_pos) = rest.find('/') {
                let ip_str = &rest[..slash_pos];
                let after_slash = &rest[slash_pos + 1..];
                let prefix_str: String =
                    after_slash.chars().take_while(|c| c.is_ascii_digit()).collect();

                if let (Ok(ip), Ok(prefix)) =
                    (ip_str.parse::<Ipv4Addr>(), prefix_str.parse::<u32>())
                {
                    if prefix > 0 && prefix <= 30 {
                        let ip_u32 = u32::from_be_bytes(ip.octets());
                        let mask = !((1u32 << (32 - prefix)) - 1);
                        let bcast = ip_u32 | !mask;
                        let bcast_ip = Ipv4Addr::from(bcast.to_be_bytes());
                        addrs.push(SocketAddr::V4(SocketAddrV4::new(bcast_ip, DISCOVERY_PORT)));
                    }
                }
            }
        }
    }
    addrs
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
    let socket = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, DISCOVERY_PORT)).await?;
    socket.set_broadcast(true)?;

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

    tokio::spawn(async move {
        // Broadcast to all subnet-specific addresses (matching node lib behavior)
        let broadcast_addrs = find_broadcast_addresses();
        let fallback =
            vec![SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::BROADCAST, DISCOVERY_PORT))];
        let targets = if broadcast_addrs.is_empty() {
            &fallback
        } else {
            &broadcast_addrs
        };
        eprintln!("[stagelinq::discovery] broadcast targets: {targets:?}");

        let mut ticker = interval(Duration::from_millis(ANNOUNCEMENT_INTERVAL_MS));
        let mut buf = [0u8; 4096];
        let mut seen: HashSet<(Ipv4Addr, u16)> = HashSet::new();

        loop {
            tokio::select! {
                _ = stop_rx.recv() => {
                    // Send logout before stopping
                    for addr in targets {
                        let _ = socket.send_to(&exit_msg, addr).await;
                    }
                    break;
                }
                _ = ticker.tick() => {
                    for addr in targets {
                        let _ = socket.send_to(&announce_msg, addr).await;
                    }
                }
                result = socket.recv_from(&mut buf) => {
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
