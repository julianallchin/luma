use std::collections::HashMap;
use std::net::Ipv4Addr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{timeout, Duration};

use crate::protocol::*;
use crate::types::*;

/// Connect to a device and discover its available services.
///
/// Protocol: connect → read messages → wait for device to send ServicesRequest →
/// send our ServicesRequest → read ServiceAnnouncement messages.
///
/// Retries up to 3 times if the device closes the connection or returns 0 services
/// (the device may need time to register our UDP announcements).
///
/// Returns the service map AND the live TCP stream (must be kept alive).
pub async fn connect_and_discover_services(
    address: Ipv4Addr,
    port: u16,
    our_token: &[u8; 16],
) -> std::io::Result<(HashMap<String, u16>, TcpStream)> {
    let addr = format!("{address}:{port}");

    for attempt in 1..=3 {
        eprintln!("[stagelinq::device] TCP connect attempt {attempt}/3 to {addr}");

        let mut stream = match timeout(
            Duration::from_millis(CONNECT_TIMEOUT_MS),
            TcpStream::connect(&addr),
        )
        .await
        {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                eprintln!("[stagelinq::device] connect error: {e}");
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
            Err(_) => {
                eprintln!("[stagelinq::device] connect timed out");
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };

        eprintln!("[stagelinq::device] TCP connected to {addr}");

        let mut services = HashMap::new();
        let mut buf = Vec::with_capacity(4096);
        let mut temp = [0u8; 4096];
        let mut service_request_sent = false;
        let mut device_closed = false;

        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);

        loop {
            if tokio::time::Instant::now() > deadline {
                break;
            }

            let read_result = timeout(Duration::from_secs(3), stream.read(&mut temp)).await;
            match read_result {
                Ok(Ok(0)) => {
                    device_closed = true;
                    break;
                }
                Ok(Ok(n)) => {
                    eprintln!(
                        "[stagelinq::device] read {n} bytes from device (buf now {})",
                        buf.len() + n
                    );
                    buf.extend_from_slice(&temp[..n]);
                }
                Ok(Err(e)) => {
                    eprintln!("[stagelinq::device] read error: {e}");
                    device_closed = true;
                    break;
                }
                Err(_) => break,
            }

            // Parse messages from buffer.
            // Main connection messages are NOT length-prefixed. Format:
            //   u32(message_id) + 16B(token) + type-specific payload
            loop {
                if buf.len() < 20 {
                    break; // Need at least msg_id(4) + token(16)
                }

                let msg_id = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);

                match MessageId::from_u32(msg_id) {
                    Some(MessageId::ServicesAnnouncement) => {
                        // 4(id) + 16(token) + utf16_string(4+N) + u16(port)
                        if buf.len() < 24 {
                            break;
                        }
                        let str_byte_len =
                            u32::from_be_bytes([buf[20], buf[21], buf[22], buf[23]]) as usize;
                        let total = 20 + 4 + str_byte_len + 2;
                        if buf.len() < total {
                            break;
                        }
                        let payload = &buf[20..total];
                        if let Ok((name, svc_port)) = parse_service_announcement_payload(payload) {
                            eprintln!(
                                "[stagelinq::device] service announced: {name} on port {svc_port}"
                            );
                            services.insert(name, svc_port);
                        }
                        buf.drain(..total);
                    }
                    Some(MessageId::TimeStamp) => {
                        let total = 44;
                        if buf.len() < total {
                            break;
                        }
                        buf.drain(..total);
                    }
                    Some(MessageId::ServicesRequest) => {
                        buf.drain(..20);
                        eprintln!("[stagelinq::device] received ServicesRequest from device");

                        if !service_request_sent {
                            eprintln!("[stagelinq::device] sending our ServicesRequest");
                            let req = build_services_request(our_token);
                            stream.write_all(&req).await?;
                            service_request_sent = true;
                        }
                    }
                    None => {
                        eprintln!(
                            "[stagelinq::device] unknown message id: {msg_id} (0x{msg_id:08x}), skipping byte"
                        );
                        buf.drain(..1);
                    }
                }
            }

            // Once we've found enough services, stop
            if service_request_sent && services.len() > 3 {
                break;
            }
        }

        if !services.is_empty() {
            eprintln!(
                "[stagelinq::device] service discovery complete: {} services found",
                services.len()
            );
            for (name, port) in &services {
                eprintln!("[stagelinq::device]   {name} => port {port}");
            }
            return Ok((services, stream));
        }

        // No services found — retry
        if device_closed {
            eprintln!("[stagelinq::device] device closed connection on attempt {attempt}, retrying in 2s...");
        } else {
            eprintln!("[stagelinq::device] 0 services on attempt {attempt}, retrying in 2s...");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::ConnectionRefused,
        "failed to discover services after 3 attempts",
    ))
}

/// Connect to a service port and send the initial service announcement handshake.
/// Includes a small delay before connecting (matching the node lib's behavior).
pub async fn connect_to_service(
    address: Ipv4Addr,
    port: u16,
    our_token: &[u8; 16],
    service_name: &str,
) -> std::io::Result<TcpStream> {
    eprintln!("[stagelinq::device] connecting to service {service_name} at {address}:{port} (waiting 500ms)");
    tokio::time::sleep(Duration::from_millis(500)).await;

    let addr = format!("{address}:{port}");
    let mut stream = timeout(
        Duration::from_millis(CONNECT_TIMEOUT_MS),
        TcpStream::connect(&addr),
    )
    .await
    .map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "service connection timed out",
        )
    })??;

    // Send service announcement
    eprintln!("[stagelinq::device] connected to {service_name}, sending announcement");
    let announcement = build_service_announcement(our_token, service_name, 0);
    stream.write_all(&announcement).await?;

    Ok(stream)
}
