use std::net::Ipv4Addr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::device::connect_to_service;
use crate::protocol::*;
use crate::types::*;

/// A beat info update from the BeatInfo service.
#[derive(Debug, Clone)]
pub struct BeatUpdate {
    pub clock: u64,
    pub decks: Vec<DeckBeatInfo>,
}

/// Run the BeatInfo service: connect, subscribe, and forward beat updates.
pub async fn run_beat_info(
    address: Ipv4Addr,
    port: u16,
    our_token: &[u8; 16],
) -> std::io::Result<(mpsc::Sender<()>, mpsc::Receiver<BeatUpdate>)> {
    let mut stream = connect_to_service(address, port, our_token, SERVICE_BEAT_INFO).await?;

    // Send subscription
    eprintln!("[stagelinq::beat_info] connected, sending subscription");
    let sub = build_beat_info_subscribe();
    stream.write_all(&sub).await?;
    eprintln!("[stagelinq::beat_info] subscription sent, listening for beat data");

    let (tx, rx) = mpsc::channel::<BeatUpdate>(256);
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
                            eprintln!("[stagelinq::beat_info] connection closed (read 0)");
                            break;
                        }
                        Ok(n) => {
                            buf.extend_from_slice(&temp[..n]);
                            // Process complete messages
                            while buf.len() >= 4 {
                                let msg_len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
                                if msg_len == 0 || buf.len() < 4 + msg_len {
                                    break;
                                }
                                let payload = buf[4..4 + msg_len].to_vec();
                                buf.drain(..4 + msg_len);

                                match parse_beat_info_message(&payload) {
                                    Err(e) => {
                                        eprintln!("[stagelinq::beat_info] parse error: {e} (payload len={})", payload.len());
                                    }
                                    Ok((_id, clock, decks)) => {
                                        let update = BeatUpdate { clock, decks };
                                        if tx.send(update).await.is_err() {
                                            return;
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
