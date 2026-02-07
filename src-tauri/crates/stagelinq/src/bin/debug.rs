//! Standalone StageLinQ debug tool.
//! Run with: cargo run -p stagelinq --bin stagelinq-debug
//!
//! Matches the node lib's socket architecture:
//! - Announce socket: ephemeral port (periodic broadcasts)
//! - Listener socket: port 51337 (receive device announcements)
//! - Both stay alive during TCP connection

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio::time::{timeout, Duration};

use stagelinq::protocol::*;
use stagelinq::types::*;

/// Find broadcast addresses for all non-loopback IPv4 interfaces.
/// Returns the first link-local (169.254.x.x) broadcast address if found,
/// otherwise the first non-loopback broadcast address.
fn find_broadcast_addr() -> Option<SocketAddr> {
    use std::process::Command;
    // Parse `ip -4 addr show` output to find broadcast addresses
    let output = Command::new("ip").args(["-4", "-o", "addr", "show"]).output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout);

    let mut link_local = None;
    let mut other = None;

    for line in text.lines() {
        // Skip loopback
        if line.contains(" lo ") {
            continue;
        }
        // Parse: "2: enp7s0    inet 169.254.1.1/16 scope global enp7s0"
        // Look for "inet ADDR/PREFIX"
        if let Some(inet_pos) = line.find("inet ") {
            let rest = &line[inet_pos + 5..];
            if let Some(slash_pos) = rest.find('/') {
                let ip_str = &rest[..slash_pos];
                // Get prefix length
                let after_slash = &rest[slash_pos + 1..];
                let prefix_str: String = after_slash.chars().take_while(|c| c.is_ascii_digit()).collect();

                if let (Ok(ip), Ok(prefix)) = (ip_str.parse::<Ipv4Addr>(), prefix_str.parse::<u32>()) {
                    if prefix > 0 && prefix <= 32 {
                        let ip_u32 = u32::from_be_bytes(ip.octets());
                        let mask = if prefix == 32 { 0xFFFFFFFF } else { !((1u32 << (32 - prefix)) - 1) };
                        let bcast = ip_u32 | !mask;
                        let bcast_ip = Ipv4Addr::from(bcast.to_be_bytes());
                        let addr = SocketAddr::V4(SocketAddrV4::new(bcast_ip, DISCOVERY_PORT));

                        if ip.octets()[0] == 169 && ip.octets()[1] == 254 {
                            link_local = Some(addr);
                        } else if other.is_none() {
                            other = Some(addr);
                        }
                    }
                }
            }
        }
    }

    link_local.or(other)
}

fn hex_dump(data: &[u8], label: &str) {
    eprintln!("--- {label} ({} bytes) ---", data.len());
    for (i, chunk) in data.chunks(16).enumerate() {
        let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
        let ascii: String = chunk
            .iter()
            .map(|&b| if (0x20..=0x7e).contains(&b) { b as char } else { '.' })
            .collect();
        eprintln!("  {:04x}: {:<48} {}", i * 16, hex.join(" "), ascii);
    }
    eprintln!("---");
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("=== StageLinQ Debug Tool ===");
    eprintln!("Using token: SoundSwitch");
    eprintln!("Identity: {SOFTWARE_SOURCE}/{SOFTWARE_NAME} v{SOFTWARE_VERSION}");
    eprintln!();

    // ---- Socket setup (matching node lib architecture) ----
    // Announce socket: ephemeral port, for broadcasting (like node's announce.js)
    let announce_socket = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)).await?;
    announce_socket.set_broadcast(true)?;
    eprintln!("Announce socket bound to: {:?}", announce_socket.local_addr()?);

    // Listener socket: port 51337, for receiving (like node's StageLinqListener)
    let listener_socket = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, DISCOVERY_PORT)).await?;
    eprintln!("Listener socket bound to: {:?}", listener_socket.local_addr()?);

    // Use subnet-specific broadcast address (like node lib does)
    // 255.255.255.255 may go out on the wrong interface on multi-homed systems
    let broadcast_addr = find_broadcast_addr().unwrap_or_else(|| {
        eprintln!("WARNING: No suitable broadcast interface found, falling back to 255.255.255.255");
        SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::BROADCAST, DISCOVERY_PORT))
    });
    eprintln!("Broadcast target: {broadcast_addr}");

    let announce_msg = build_discovery_message(
        &SOUNDSWITCH_TOKEN,
        SOFTWARE_SOURCE,
        ACTION_LOGIN,
        SOFTWARE_NAME,
        SOFTWARE_VERSION,
        0,
    );
    hex_dump(&announce_msg, "Our discovery announcement");

    // Phase 1: Discovery
    eprintln!();
    eprintln!("== Phase 1: UDP Discovery ==");

    // Send initial announcement
    announce_socket.send_to(&announce_msg, broadcast_addr).await?;
    eprintln!("Sent discovery announcement via announce socket");

    // Start periodic announcements in background
    let announce_socket_clone = std::sync::Arc::new(announce_socket);
    let announce_bg = announce_socket_clone.clone();
    let announce_msg_clone = announce_msg.clone();
    let _announce_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(ANNOUNCEMENT_INTERVAL_MS));
        loop {
            interval.tick().await;
            let _ = announce_bg.send_to(&announce_msg_clone, broadcast_addr).await;
        }
    });

    // Listen for device announcements
    let mut buf = [0u8; 4096];
    let mut device_addr = None;
    let mut device_port = 0u16;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);

    while tokio::time::Instant::now() < deadline {
        let recv_result = timeout(Duration::from_secs(2), listener_socket.recv_from(&mut buf)).await;

        match recv_result {
            Ok(Ok((len, addr))) => {
                let data = &buf[..len];

                match parse_discovery_message(data) {
                    Ok(msg) => {
                        if msg.source == SOFTWARE_SOURCE {
                            continue; // our own
                        }
                        if msg.action == ACTION_LOGOUT || msg.port == 0 {
                            continue;
                        }
                        if msg.software_name == "OfflineAnalyzer"
                            || msg.software_name.starts_with("SoundSwitch")
                            || msg.software_name == "JM08"
                            || msg.software_name == "SSS0"
                        {
                            eprintln!("  Skipping: {} ({})", msg.software_name, msg.source);
                            continue;
                        }

                        let ip = match addr {
                            SocketAddr::V4(v4) => *v4.ip(),
                            _ => continue,
                        };
                        eprintln!(
                            "  ** PLAYER FOUND: {} {} v{} at {}:{}",
                            msg.software_name, msg.source, msg.software_version, ip, msg.port
                        );
                        eprintln!("  Token: {:02x?}", &msg.token);
                        device_addr = Some(ip);
                        device_port = msg.port;
                        break;
                    }
                    Err(e) => {
                        eprintln!("  Parse error from {addr}: {e}");
                        hex_dump(data, &format!("raw UDP from {addr}"));
                    }
                }
            }
            Ok(Err(e)) => eprintln!("recv error: {e}"),
            Err(_) => eprintln!("  (waiting for devices...)"),
        }
    }

    let device_ip = match device_addr {
        Some(ip) => ip,
        None => {
            eprintln!("No player device found after 10s. Exiting.");
            return Ok(());
        }
    };

    // NOTE: Keep both UDP sockets alive! Don't drop them.
    // The node lib keeps announcing and listening during TCP.

    let addr_str = format!("{device_ip}:{device_port}");

    // The node lib's first TCP attempt typically fails because the device hasn't
    // fully registered the client yet. The node lib's requestAllServicePorts waits
    // 5s before timing out, giving the device time to see more announcements.
    // We replicate this: wait 2s initially, then retry with delays.
    eprintln!("Waiting 2s for announcements to reach device...");
    tokio::time::sleep(Duration::from_secs(2)).await;

    let mut stream: Option<TcpStream> = None;
    let mut initial_bytes: Vec<u8> = Vec::new();
    for attempt in 1..=5 {
        eprintln!();
        eprintln!("== Phase 2: TCP connect attempt {attempt}/5 to {addr_str} ==");

        match timeout(Duration::from_secs(5), TcpStream::connect(&addr_str)).await {
            Ok(Ok(mut s)) => {
                eprintln!("TCP connected. Local addr: {:?}", s.local_addr());
                // Wait up to 5s for first byte (matching node lib's LISTEN_TIMEOUT)
                let mut test_buf = [0u8; 4096];
                match timeout(Duration::from_secs(5), s.read(&mut test_buf)).await {
                    Ok(Ok(0)) => {
                        eprintln!("!! Device closed connection (attempt {attempt}).");
                    }
                    Ok(Ok(n)) => {
                        eprintln!("Got {n} bytes on first read! Device is talking.");
                        hex_dump(&test_buf[..n], "first TCP data");
                        initial_bytes.extend_from_slice(&test_buf[..n]);
                        stream = Some(s);
                        break;
                    }
                    Ok(Err(e)) => {
                        eprintln!("Read error: {e}");
                    }
                    Err(_) => {
                        eprintln!("5s timeout - device connected but silent.");
                        stream = Some(s);
                        break;
                    }
                }
                eprintln!("   Sleeping 1s before retry...");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Ok(Err(e)) => {
                eprintln!("TCP connect error: {e}");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Err(_) => {
                eprintln!("TCP connect timed out.");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }

    let mut stream = match stream {
        Some(s) => s,
        None => {
            eprintln!("All 5 TCP attempts failed. Exiting.");
            return Ok(());
        }
    };

    eprintln!();
    eprintln!("== Phase 3: Reading device data ==");
    eprintln!("(Device should send TimeStamp, ServicesAnnouncement, ServicesRequest)");
    eprintln!();

    // Pre-fill with any bytes read during connect phase
    let mut tcp_buf = Vec::with_capacity(8192);
    tcp_buf.extend_from_slice(&initial_bytes);
    let mut temp = [0u8; 4096];
    let mut services_request_received = false;
    let mut service_count = 0u32;
    let tcp_deadline = tokio::time::Instant::now() + Duration::from_secs(30);

    loop {
        if tokio::time::Instant::now() > tcp_deadline {
            eprintln!("TCP read deadline reached (30s).");
            break;
        }

        let read_result = timeout(Duration::from_secs(5), stream.read(&mut temp)).await;
        match read_result {
            Ok(Ok(0)) => {
                eprintln!("!! Device closed connection (read 0).");
                break;
            }
            Ok(Ok(n)) => {
                hex_dump(&temp[..n], &format!("TCP read ({n} bytes)"));
                tcp_buf.extend_from_slice(&temp[..n]);

                // Parse messages
                loop {
                    if tcp_buf.len() < 20 {
                        break;
                    }

                    let msg_id = u32::from_be_bytes([tcp_buf[0], tcp_buf[1], tcp_buf[2], tcp_buf[3]]);

                    match MessageId::from_u32(msg_id) {
                        Some(MessageId::ServicesAnnouncement) => {
                            if tcp_buf.len() < 24 { break; }
                            let str_byte_len = u32::from_be_bytes([tcp_buf[20], tcp_buf[21], tcp_buf[22], tcp_buf[23]]) as usize;
                            let total = 20 + 4 + str_byte_len + 2;
                            if tcp_buf.len() < total { break; }
                            let payload = &tcp_buf[20..total];
                            match parse_service_announcement_payload(payload) {
                                Ok((name, port)) => {
                                    eprintln!("  ** SERVICE: {name} on port {port}");
                                    service_count += 1;
                                }
                                Err(e) => {
                                    eprintln!("  ServicesAnnouncement parse error: {e}");
                                    hex_dump(payload, "announcement payload");
                                }
                            }
                            tcp_buf.drain(..total);
                        }
                        Some(MessageId::TimeStamp) => {
                            let total = 44;
                            if tcp_buf.len() < total { break; }
                            let ns = u64::from_be_bytes(tcp_buf[36..44].try_into().unwrap());
                            eprintln!("  TimeStamp: {}s alive", ns / 1_000_000_000);
                            tcp_buf.drain(..total);
                        }
                        Some(MessageId::ServicesRequest) => {
                            eprintln!("  ** ServicesRequest from device!");
                            tcp_buf.drain(..20);
                            services_request_received = true;

                            let req = build_services_request(&SOUNDSWITCH_TOKEN);
                            hex_dump(&req, "Sending our ServicesRequest");
                            stream.write_all(&req).await?;
                            eprintln!("  Sent our ServicesRequest with SoundSwitch token");
                        }
                        None => {
                            eprintln!("  Unknown msg_id: {} (0x{:08x})", msg_id, msg_id);
                            hex_dump(&tcp_buf[..tcp_buf.len().min(64)], "buffer head");
                            tcp_buf.drain(..1);
                        }
                    }
                }

                // If we got services, wait a bit for more then stop
                if services_request_received && service_count > 3 {
                    eprintln!("Got {} services, waiting 2s for more...", service_count);
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    // Drain remaining
                    if let Ok(Ok(n)) = timeout(Duration::from_millis(500), stream.read(&mut temp)).await {
                        if n > 0 {
                            tcp_buf.extend_from_slice(&temp[..n]);
                            hex_dump(&temp[..n], &format!("final TCP read ({n} bytes)"));
                        }
                    }
                    break;
                }
            }
            Ok(Err(e)) => {
                eprintln!("TCP read error: {e}");
                break;
            }
            Err(_) => {
                eprintln!("TCP read timeout (5s). Total bytes so far: {}", tcp_buf.len());
            }
        }
    }

    eprintln!();
    eprintln!("== Summary ==");
    eprintln!("ServicesRequest received: {services_request_received}");
    eprintln!("Services discovered: {service_count}");

    if service_count == 0 {
        eprintln!("No services found. Exiting.");
        return Ok(());
    }

    // Now use the crate's proper device module to do service discovery + connect to services
    // We need to reconnect since we consumed the main connection for raw debugging.
    // Drop the old stream and use the high-level API.
    drop(stream);

    eprintln!();
    eprintln!("== Phase 4: Reconnect using crate API for StateMap + BeatInfo ==");

    let token = SOUNDSWITCH_TOKEN;
    let (svc_map, _main_conn) = stagelinq::device::connect_and_discover_services(device_ip, device_port, &token).await?;

    eprintln!("Services: {svc_map:?}");

    // Connect to StateMap
    let state_map_port = svc_map.get(SERVICE_STATE_MAP).copied();
    let beat_info_port = svc_map.get(SERVICE_BEAT_INFO).copied();

    // Must keep stop handles alive so the service tasks don't exit
    let mut _state_stop = None;
    let mut _beat_stop = None;

    let mut state_rx = if let Some(port) = state_map_port {
        let paths: Vec<String> = stagelinq::types::deck_state_paths(1)
            .into_iter()
            .chain(stagelinq::types::deck_state_paths(2))
            .chain(stagelinq::types::mixer_state_paths())
            .collect();
        match stagelinq::services::state_map::run_state_map(device_ip, port, &token, &paths).await {
            Ok((stop, rx)) => {
                eprintln!("[StateMap] connected, subscribed to {} paths", paths.len());
                _state_stop = Some(stop);
                Some(rx)
            }
            Err(e) => {
                eprintln!("[StateMap] FAILED: {e}");
                None
            }
        }
    } else {
        eprintln!("[StateMap] not available");
        None
    };

    let mut beat_rx = if let Some(port) = beat_info_port {
        match stagelinq::services::beat_info::run_beat_info(device_ip, port, &token).await {
            Ok((stop, rx)) => {
                eprintln!("[BeatInfo] connected");
                _beat_stop = Some(stop);
                Some(rx)
            }
            Err(e) => {
                eprintln!("[BeatInfo] FAILED: {e}");
                None
            }
        }
    } else {
        eprintln!("[BeatInfo] not available");
        None
    };

    eprintln!();
    eprintln!("== Phase 5: Live data (15s) ==");
    let live_deadline = tokio::time::Instant::now() + Duration::from_secs(15);

    loop {
        if tokio::time::Instant::now() > live_deadline {
            eprintln!("Live data window complete.");
            break;
        }

        tokio::select! {
            state = async {
                match state_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                match state {
                    Some(change) => {
                        eprintln!("[STATE] {} = {}", change.path, change.value);
                    }
                    None => {
                        eprintln!("[STATE] channel closed");
                        state_rx = None;
                    }
                }
            }
            beat = async {
                match beat_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                match beat {
                    Some(update) => {
                        let decks: Vec<String> = update.decks.iter().enumerate().map(|(i, d)| {
                            format!("[{}] beat={:.1} bpm={:.1}", i, d.beat, d.bpm)
                        }).collect();
                        eprintln!("[BEAT] {}", decks.join(" | "));
                    }
                    None => {
                        eprintln!("[BEAT] channel closed");
                        beat_rx = None;
                    }
                }
            }
        }

        if state_rx.is_none() && beat_rx.is_none() {
            eprintln!("Both channels closed.");
            break;
        }
    }

    // Keep listener alive
    eprintln!("Listener socket still on: {:?}", listener_socket.local_addr());
    eprintln!();
    eprintln!("=== Done ===");
    Ok(())
}
