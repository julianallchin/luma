use std::net::Ipv4Addr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::device::connect_to_service;
use crate::protocol::*;
use crate::types::*;

/// A state change received from the StateMap service.
#[derive(Debug, Clone)]
pub struct StateChange {
    pub path: String,
    pub value: serde_json::Value,
}

/// Run the StateMap service: connect, subscribe to all paths, and forward state changes.
///
/// Returns a handle to stop the service and a receiver for state changes.
pub async fn run_state_map(
    address: Ipv4Addr,
    port: u16,
    our_token: &[u8; 16],
    paths: &[String],
) -> std::io::Result<(mpsc::Sender<()>, mpsc::Receiver<StateChange>)> {
    let mut stream = connect_to_service(address, port, our_token, SERVICE_STATE_MAP).await?;

    eprintln!("[stagelinq::state_map] subscribing to {} paths", paths.len());
    // Subscribe to all paths
    for path in paths {
        let msg = build_statemap_subscribe(path);
        stream.write_all(&msg).await?;
    }
    eprintln!("[stagelinq::state_map] all subscriptions sent, listening for updates");

    let (tx, rx) = mpsc::channel::<StateChange>(256);
    let (stop_tx, mut stop_rx) = mpsc::channel::<()>(1);

    tokio::spawn(async move {
        let mut buf = Vec::with_capacity(8192);
        let mut temp = [0u8; 4096];

        loop {
            tokio::select! {
                _ = stop_rx.recv() => break,
                result = stream.read(&mut temp) => {
                    match result {
                        Ok(0) => {
                            eprintln!("[stagelinq::state_map] connection closed (read 0)");
                            break;
                        }
                        Ok(n) => {
                            buf.extend_from_slice(&temp[..n]);
                            // Process complete messages from buffer
                            while buf.len() >= 4 {
                                let msg_len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
                                if msg_len == 0 || buf.len() < 4 + msg_len {
                                    break; // incomplete message
                                }
                                let payload = buf[4..4 + msg_len].to_vec();
                                buf.drain(..4 + msg_len);

                                match parse_statemap_message(&payload) {
                                    Err(e) => {
                                        eprintln!("[stagelinq::state_map] parse error: {e} (payload len={})", payload.len());
                                    }
                                    Ok(value) => {
                                        let change = match value {
                                            StateMapValue::Json { name, value } => {
                                                StateChange { path: name, value }
                                            }
                                            StateMapValue::Interval { name, interval } => {
                                                StateChange {
                                                    path: name,
                                                    value: serde_json::Value::Number(
                                                        serde_json::Number::from(interval),
                                                    ),
                                                }
                                            }
                                        };
                                        if tx.send(change).await.is_err() {
                                            return; // receiver dropped
                                        }
                                    }
                                }
                            }
                        }
                        Err(_) => break,
                    }
                }
            }
        }
    });

    Ok((stop_tx, rx))
}
